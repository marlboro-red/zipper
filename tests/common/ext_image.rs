#![allow(dead_code)]

fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

pub fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

pub fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

pub fn crc32c(seed: u32, bytes: &[u8]) -> u32 {
    let mut crc = seed;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x82F6_3B78
            } else {
                crc >> 1
            };
        }
    }
    crc
}

pub fn crc16(seed: u16, bytes: &[u8]) -> u16 {
    let mut crc = seed;
    for byte in bytes {
        crc ^= u16::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
    }
    crc
}

pub const SUPERBLOCK: usize = 1024;

pub struct InodeBlocks {
    pub data: Vec<u64>,
    pub maps: Vec<(u64, Region)>,
}

pub struct RecordAt {
    pub offset: usize,
    pub block: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub block_size: usize,
    pub inode_size: usize,
    pub inodes_per_group: u32,
    pub groups: u32,
    pub descriptor_size: usize,
    pub first_data_block: u64,
    pub metadata_csum: bool,
    pub gdt_csum: bool,
    pub wide: bool,
    pub seed: u32,
    pub meta_bg: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Superblock,
    Descriptor(u32),
    Inode(u32),
    ExtentBlock(u32),
    IndirectBlock,
    FolderBlock(u32),
}

impl Layout {
    pub fn parse(image: &[u8]) -> Layout {
        let sb = &image[SUPERBLOCK..SUPERBLOCK + 1024];
        assert_eq!(le16(sb, 0x38), 0xEF53);
        let block_size = 1024usize << le32(sb, 0x18);
        let incompat = le32(sb, 0x60);
        let ro_compat = le32(sb, 0x64);
        let wide = incompat & 0x80 != 0;
        let blocks = u64::from(le32(sb, 4))
            | if wide {
                u64::from(le32(sb, 0x150)) << 32
            } else {
                0
            };
        let first_data_block = u64::from(le32(sb, 0x14));
        let per_group = u64::from(le32(sb, 0x20));
        let seed = if incompat & 0x2000 != 0 {
            le32(sb, 0x270)
        } else {
            crc32c(u32::MAX, &sb[0x68..0x78])
        };
        Layout {
            block_size,
            inode_size: if le32(sb, 0x4C) == 0 {
                128
            } else {
                usize::from(le16(sb, 0x58))
            },
            inodes_per_group: le32(sb, 0x28),
            groups: (blocks - first_data_block).div_ceil(per_group) as u32,
            descriptor_size: if wide {
                usize::from(le16(sb, 0xFE))
            } else {
                32
            },
            first_data_block,
            metadata_csum: ro_compat & 0x400 != 0,
            gdt_csum: ro_compat & 0x10 != 0,
            wide,
            seed,
            meta_bg: incompat & 0x10 != 0,
        }
    }

    pub fn descriptor_at(&self, group: u32) -> usize {
        assert!(
            !self.meta_bg,
            "the test reader places descriptors without meta_bg only"
        );
        (self.first_data_block as usize + 1) * self.block_size
            + group as usize * self.descriptor_size
    }

    pub fn inode_table(&self, image: &[u8], group: u32) -> u64 {
        let at = self.descriptor_at(group);
        u64::from(le32(image, at + 8))
            | if self.wide {
                u64::from(le32(image, at + 0x28)) << 32
            } else {
                0
            }
    }

    pub fn inode_at(&self, image: &[u8], number: u32) -> usize {
        let group = (number - 1) / self.inodes_per_group;
        let index = ((number - 1) % self.inodes_per_group) as usize;
        self.inode_table(image, group) as usize * self.block_size + index * self.inode_size
    }

    fn inode_in_use(&self, image: &[u8], number: u32) -> bool {
        let group = (number - 1) / self.inodes_per_group;
        let at = self.descriptor_at(group);
        if (self.gdt_csum || self.metadata_csum) && le16(image, at + 0x12) & 1 != 0 {
            return false;
        }
        let bitmap = u64::from(le32(image, at + 4))
            | if self.wide {
                u64::from(le32(image, at + 0x24)) << 32
            } else {
                0
            };
        let index = ((number - 1) % self.inodes_per_group) as usize;
        image[bitmap as usize * self.block_size + index / 8] & (1 << (index % 8)) != 0
    }

    pub fn used_inodes(&self, image: &[u8]) -> Vec<u32> {
        (1..=self.groups * self.inodes_per_group)
            .filter(|&number| number >= 11 || number == 2 || number == 8)
            .filter(|&number| self.inode_in_use(image, number))
            .filter(|&number| le16(image, self.inode_at(image, number)) != 0)
            .collect()
    }

    pub fn mode(&self, image: &[u8], number: u32) -> u16 {
        le16(image, self.inode_at(image, number))
    }

    pub fn size(&self, image: &[u8], number: u32) -> u64 {
        let at = self.inode_at(image, number);
        u64::from(le32(image, at + 4)) | u64::from(le32(image, at + 0x6C)) << 32
    }

    pub fn blocks(&self, image: &[u8], number: u32) -> InodeBlocks {
        let at = self.inode_at(image, number);
        let flags = le32(image, at + 0x20);
        let size = self.size(image, number);
        let needed = size.div_ceil(self.block_size as u64);
        let mut data = Vec::new();
        let mut maps = Vec::new();
        if flags & 0x1000_0000 != 0 {
            return InodeBlocks { data, maps };
        }
        let mode = self.mode(image, number) & 0o170_000;
        if mode == 0o120_000 && flags & 0x8_0000 == 0 && size < 60 {
            return InodeBlocks { data, maps };
        }
        if flags & 0x8_0000 != 0 {
            let root = image[at + 0x28..at + 0x64].to_vec();
            self.extent_node(image, number, &root, &mut data, &mut maps);
        } else {
            let pointers = &image[at + 0x28..at + 0x64];
            let per_block = (self.block_size / 4) as u64;
            let mut logical = 0u64;
            for index in 0..15usize {
                let level = index.saturating_sub(11) as u32;
                let pointer = le32(pointers, index * 4);
                self.indirect(image, pointer, level, logical, needed, &mut data, &mut maps);
                logical += per_block.pow(level);
            }
        }
        InodeBlocks { data, maps }
    }

    fn extent_node(
        &self,
        image: &[u8],
        number: u32,
        node: &[u8],
        data: &mut Vec<u64>,
        maps: &mut Vec<(u64, Region)>,
    ) {
        let entries = usize::from(le16(node, 2));
        let depth = le16(node, 6);
        for index in 0..entries {
            let entry = &node[12 + 12 * index..24 + 12 * index];
            if depth == 0 {
                let length = u64::from(le16(entry, 4) & 0x7FFF).max(1);
                let start = u64::from(le32(entry, 8)) | u64::from(le16(entry, 6)) << 32;
                data.extend(start..start + length);
            } else {
                let child = u64::from(le32(entry, 4)) | u64::from(le16(entry, 8)) << 32;
                maps.push((child, Region::ExtentBlock(number)));
                let at = child as usize * self.block_size;
                let block = image[at..at + self.block_size].to_vec();
                self.extent_node(image, number, &block, data, maps);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn indirect(
        &self,
        image: &[u8],
        pointer: u32,
        level: u32,
        logical: u64,
        needed: u64,
        data: &mut Vec<u64>,
        maps: &mut Vec<(u64, Region)>,
    ) {
        if pointer == 0 || logical >= needed {
            return;
        }
        if level == 0 {
            data.push(u64::from(pointer));
            return;
        }
        maps.push((u64::from(pointer), Region::IndirectBlock));
        let at = pointer as usize * self.block_size;
        let span = ((self.block_size / 4) as u64).pow(level - 1);
        for index in 0..self.block_size / 4 {
            let child = le32(image, at + 4 * index);
            self.indirect(
                image,
                child,
                level - 1,
                logical + index as u64 * span,
                needed,
                data,
                maps,
            );
        }
    }

    pub fn regions(&self, image: &[u8]) -> Vec<(usize, usize, Region)> {
        let mut found = vec![(SUPERBLOCK, SUPERBLOCK + 1024, Region::Superblock)];
        for group in 0..self.groups {
            let at = self.descriptor_at(group);
            found.push((at, at + self.descriptor_size, Region::Descriptor(group)));
        }
        for number in self.used_inodes(image) {
            let at = self.inode_at(image, number);
            found.push((at, at + self.inode_size, Region::Inode(number)));
            let InodeBlocks { data, maps } = self.blocks(image, number);
            for (block, region) in maps {
                let at = block as usize * self.block_size;
                found.push((at, at + self.block_size, region));
            }
            if self.mode(image, number) & 0o170_000 == 0o040_000 {
                for block in data {
                    let at = block as usize * self.block_size;
                    found.push((at, at + self.block_size, Region::FolderBlock(number)));
                }
            }
        }
        found
    }

    fn generation(&self, image: &[u8], number: u32) -> u32 {
        le32(image, self.inode_at(image, number) + 0x64)
    }

    fn inode_seed(&self, image: &[u8], number: u32) -> u32 {
        crc32c(
            crc32c(self.seed, &number.to_le_bytes()),
            &self.generation(image, number).to_le_bytes(),
        )
    }

    pub fn seal_superblock(&self, image: &mut [u8]) {
        if self.metadata_csum {
            let crc = crc32c(u32::MAX, &image[SUPERBLOCK..SUPERBLOCK + 0x3FC]);
            put32(image, SUPERBLOCK + 0x3FC, crc);
        }
    }

    pub fn seal_descriptor(&self, image: &mut [u8], group: u32) {
        let at = self.descriptor_at(group);
        let mut bytes = image[at..at + self.descriptor_size].to_vec();
        bytes[0x1E..0x20].fill(0);
        let number = group.to_le_bytes();
        let crc = if self.metadata_csum {
            crc32c(crc32c(self.seed, &number), &bytes) as u16
        } else if self.gdt_csum {
            let uuid = &image[SUPERBLOCK + 0x68..SUPERBLOCK + 0x78];
            let crc = crc16(crc16(crc16(u16::MAX, uuid), &number), &bytes[..0x1E]);
            if bytes.len() > 0x20 {
                crc16(crc, &bytes[0x20..])
            } else {
                crc
            }
        } else {
            return;
        };
        put16(image, at + 0x1E, crc);
    }

    pub fn seal_inode(&self, image: &mut [u8], number: u32) {
        if !self.metadata_csum {
            return;
        }
        let at = self.inode_at(image, number);
        let mut bytes = image[at..at + self.inode_size].to_vec();
        bytes[0x7C..0x7E].fill(0);
        let wide = self.inode_size > 128 && 128 + usize::from(le16(&bytes, 0x80)) >= 0x84;
        if wide {
            bytes[0x82..0x84].fill(0);
        }
        let crc = crc32c(self.inode_seed(image, number), &bytes);
        put16(image, at + 0x7C, crc as u16);
        if wide {
            put16(image, at + 0x82, (crc >> 16) as u16);
        }
    }

    pub fn seal_extent_block(&self, image: &mut [u8], number: u32, block: u64) {
        if !self.metadata_csum {
            return;
        }
        let at = block as usize * self.block_size;
        let tail = 12 + 12 * usize::from(le16(image, at + 4));
        let crc = crc32c(self.inode_seed(image, number), &image[at..at + tail]);
        put32(image, at + tail, crc);
    }

    pub fn seal_folder_block(&self, image: &mut [u8], number: u32, block: u64) {
        if !self.metadata_csum {
            return;
        }
        let at = block as usize * self.block_size;
        let end = at + self.block_size - 12;
        if le16(image, end + 4) != 12 || image[end + 7] != 0xDE {
            return;
        }
        let crc = crc32c(self.inode_seed(image, number), &image[at..end]);
        put32(image, end + 8, crc);
    }

    pub fn seal(&self, image: &mut [u8], region: Region, start: usize) {
        match region {
            Region::Superblock => self.seal_superblock(image),
            Region::Descriptor(group) => self.seal_descriptor(image, group),
            Region::Inode(number) => self.seal_inode(image, number),
            Region::ExtentBlock(number) => {
                self.seal_extent_block(image, number, (start / self.block_size) as u64)
            }
            Region::IndirectBlock => {}
            Region::FolderBlock(number) => {
                self.seal_folder_block(image, number, (start / self.block_size) as u64)
            }
        }
    }

    pub fn record(&self, image: &[u8], folder: u32, name: &str) -> RecordAt {
        let data = self.blocks(image, folder).data;
        for block in data {
            let start = block as usize * self.block_size;
            let mut at = start;
            while at < start + self.block_size {
                let length = usize::from(le16(image, at + 4));
                let name_length = usize::from(image[at + 6]);
                if le32(image, at) != 0 && &image[at + 8..at + 8 + name_length] == name.as_bytes() {
                    return RecordAt { offset: at, block };
                }
                assert!(length >= 8, "bad record length in folder {folder}");
                at += length;
            }
        }
        panic!("folder {folder} holds no {name:?}");
    }

    pub fn lookup(&self, image: &[u8], path: &str) -> u32 {
        let mut inode = 2;
        for part in path.split('/') {
            inode = le32(image, self.record(image, inode, part).offset);
        }
        inode
    }
}
