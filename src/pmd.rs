use crate::{BadField, EndWatched, Field, field_check};
use std::io::{self, BufRead, Read};

pub(crate) const SIGNATURE: [u8; 4] = 0x84ac_af8fu32.to_le_bytes();
pub(crate) const HEADER: usize = 16;
// EXTERNAL(7-Zip PpmdHandler.cpp, CItem::ReadHeader): versions 6 to 11 parse, a name holds at
// most 512 bytes, and from version 8 the top two bits of the name length give the restore method.
const OLDEST_VERSION: u16 = 6;
const NEWEST_VERSION: u16 = 11;
const FIRST_VERSION_WITH_RESTORE: u16 = 8;
const LONGEST_NAME: u16 = 512;
const RESTORE_SHIFT: u16 = 14;
const NAME_LENGTH_MASK: u16 = (1 << RESTORE_SHIFT) - 1;
const LARGEST_RESTORE: u16 = 2;
const VERSION_H: u16 = 7;
const VERSION_I: u16 = 8;
const SMALLEST_ORDER: u16 = 2;
const RESTORE_UNSUPPORTED: u16 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Model {
    H,
    I { cut_off: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub(crate) version: u16,
    pub(crate) order: u16,
    pub(crate) memory_mib: u16,
    pub(crate) restore: u16,
    pub(crate) name_length: u16,
}

pub(crate) fn header(bytes: [u8; HEADER]) -> Field<Header> {
    field_check(
        bytes[..SIGNATURE.len()] == SIGNATURE,
        "the file does not start with the PPMd signature",
    )?;
    let info = u16::from_le_bytes([bytes[8], bytes[9]]);
    let version = info >> 12;
    field_check(
        (OLDEST_VERSION..=NEWEST_VERSION).contains(&version),
        "the PPMd file names a version 7-Zip does not know",
    )?;
    let mut name_length = u16::from_le_bytes([bytes[10], bytes[11]]);
    let restore = name_length >> RESTORE_SHIFT;
    field_check(
        restore <= LARGEST_RESTORE,
        "the PPMd restore method is unknown",
    )?;
    if version >= FIRST_VERSION_WITH_RESTORE {
        name_length &= NAME_LENGTH_MASK;
    }
    field_check(
        name_length <= LONGEST_NAME,
        "the PPMd file name is longer than 512 bytes",
    )?;
    Ok(Header {
        version,
        order: (info & 0x0f) + 1,
        memory_mib: ((info >> 4) & 0xff) + 1,
        restore,
        name_length,
    })
}

impl Header {
    // EXTERNAL(7-Zip PpmdHandler.cpp, CItem::IsSupported): 7-Zip decodes only var.H as version 7
    // and var.I as version 8, from order 2, and var.I without its unsupported restore method.
    pub(crate) fn model(&self) -> Field<Model> {
        field_check(
            self.order >= SMALLEST_ORDER,
            "the PPMd model order is below 2",
        )?;
        match self.version {
            VERSION_H => Ok(Model::H),
            VERSION_I if self.restore < RESTORE_UNSUPPORTED => Ok(Model::I {
                cut_off: self.restore == 1,
            }),
            VERSION_I => Err(BadField("the PPMd var.I restore method is unsupported")),
            _ => Err(BadField(
                "the PPMd file is a version other than var.H or var.I",
            )),
        }
    }

    pub(crate) fn memory(&self) -> u32 {
        u32::from(self.memory_mib) << 20
    }
}

// EXTERNAL(7-Zip PpmdHandler.cpp, CItem::ReadHeader): the stored name ends at its first NUL.
pub(crate) fn stored_name(name: &[u8]) -> &[u8] {
    name.split(|&byte| byte == 0).next().unwrap_or_default()
}

fn data_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, BadField(message))
}

enum Decoder<R: Read> {
    H(Box<ppmd_rust::Ppmd7aDecoder<EndWatched<R>>>),
    I(Box<ppmd_rust::Ppmd8Decoder<EndWatched<R>>>),
}

enum State<R: Read> {
    Unstarted { input: R, header: Header },
    Started(Decoder<R>),
    Failed,
}

pub(crate) struct PmdReader<R: Read> {
    state: State<R>,
    checked_end: bool,
}

impl<R: BufRead> PmdReader<R> {
    pub(crate) fn new(input: R, header: &Header) -> io::Result<Self> {
        header
            .model()
            .map_err(|BadField(message)| data_error(message))?;
        Ok(PmdReader {
            state: State::Unstarted {
                input,
                header: *header,
            },
            checked_end: false,
        })
    }

    fn decoder(&mut self) -> io::Result<&mut Decoder<R>> {
        if let State::Unstarted { .. } = self.state {
            let State::Unstarted { input, header } =
                std::mem::replace(&mut self.state, State::Failed)
            else {
                unreachable!("the state was just matched");
            };
            self.state = State::Started(started(input, &header)?);
        }
        match &mut self.state {
            State::Started(decoder) => Ok(decoder),
            _ => Err(data_error("the PPMd decoder failed to start")),
        }
    }
}

fn started<R: BufRead>(input: R, header: &Header) -> io::Result<Decoder<R>> {
    let model = header
        .model()
        .map_err(|BadField(message)| data_error(message))?;
    let input = EndWatched {
        inner: input,
        ran_out: false,
    };
    let order = u32::from(header.order);
    let started = match model {
        Model::H => ppmd_rust::Ppmd7aDecoder::new(input, order, header.memory())
            .map(|decoder| Decoder::H(Box::new(decoder))),
        Model::I { cut_off } => ppmd_rust::Ppmd8Decoder::new(
            input,
            order,
            header.memory(),
            if cut_off {
                ppmd_rust::RestoreMethod::CutOff
            } else {
                ppmd_rust::RestoreMethod::Restart
            },
        )
        .map(|decoder| Decoder::I(Box::new(decoder))),
    };
    started.map_err(|error| match error {
        ppmd_rust::Error::IoError(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            data_error("the PPMd stream is truncated")
        }
        ppmd_rust::Error::IoError(error) => error,
        ppmd_rust::Error::MemoryAllocation => {
            io::Error::new(io::ErrorKind::OutOfMemory, "cannot allocate the PPMd model")
        }
        _ => data_error("cannot start the PPMd decoder"),
    })
}

impl<R: BufRead> Read for PmdReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.checked_end {
            return Ok(0);
        }
        let decoder = self.decoder()?;
        let read = match decoder {
            Decoder::H(decoder) => decoder.read(out)?,
            Decoder::I(decoder) => decoder.read(out)?,
        };
        if read > 0 {
            return Ok(read);
        }
        let input = match decoder {
            Decoder::H(decoder) => decoder.get_mut(),
            Decoder::I(decoder) => decoder.get_mut(),
        };
        if input.ran_out {
            return Err(data_error("the PPMd stream is truncated"));
        }
        if !input.inner.fill_buf()?.is_empty() {
            return Err(data_error(
                "the PPMd file has data after its end marker, such as a second file, which Zipper does not read",
            ));
        }
        self.checked_end = true;
        Ok(0)
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn pmd_headers_give_the_decoder_only_parameters_it_accepts() {
        let bytes: [u8; HEADER] = kani::any();
        let Ok(header) = header(bytes) else {
            return;
        };
        assert!(bytes[..4] == SIGNATURE);
        assert!((6..=11).contains(&header.version));
        assert!((1..=16).contains(&header.order) && (1..=256).contains(&header.memory_mib));
        assert!(header.name_length <= 512 && header.restore <= 2);
        assert!(header.version >= 8 || header.restore == 0);
        if let Ok(model) = header.model() {
            assert!(header.order >= 2);
            assert!(header.memory() >= 1 << 20 && header.memory() <= 256 << 20);
            match model {
                Model::H => assert!(header.version == 7),
                Model::I { cut_off } => {
                    assert!(header.version == 8 && header.restore < 2);
                    assert!(cut_off == (header.restore == 1));
                }
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(6)]
    fn stored_names_end_before_their_first_nul() {
        let bytes: [u8; 4] = kani::any();
        let name = stored_name(&bytes);
        assert!(name.len() <= bytes.len() && name == &bytes[..name.len()]);
        assert!(!name.contains(&0));
        assert!(name.len() == bytes.len() || bytes[name.len()] == 0);
    }
}
