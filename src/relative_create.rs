use std::{
    collections::HashMap,
    fs::File,
    io,
    os::windows::{
        ffi::OsStrExt,
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Path, PathBuf},
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_CREATE, FILE_NON_DIRECTORY_FILE, FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
        },
    },
    Win32::{
        Foundation::{
            CloseHandle, HANDLE, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING,
        },
        Storage::FileSystem::{
            FILE_ATTRIBUTE_NORMAL, FILE_FLAG_BACKUP_SEMANTICS, FILE_GENERIC_WRITE,
            FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            FILE_TRAVERSE,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

const SYNCHRONIZE: u32 = 0x0010_0000;

// RECORD(benchmarks/native-zip/relative-create/README.md): opt-in. A name relative to an open
// folder cut Defender's name normalization on one machine and changed nothing on another.
pub(crate) struct Folders(HashMap<PathBuf, File>);

impl Folders {
    pub(crate) fn open(folders: impl IntoIterator<Item = PathBuf>) -> io::Result<Self> {
        let mut open = HashMap::new();
        for folder in folders {
            if open.contains_key(&folder) {
                continue;
            }
            let handle = File::options()
                .access_mode(FILE_TRAVERSE | SYNCHRONIZE)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(&folder)?;
            open.insert(folder, handle);
        }
        Ok(Self(open))
    }

    pub(crate) fn create_new(&self, path: &Path, attributes: u32) -> Option<io::Result<File>> {
        let folder = self.0.get(path.parent()?)?;
        let leaf: Vec<u16> = path.file_name()?.encode_wide().collect();
        let bytes = u16::try_from(leaf.len() * 2).ok()?;
        let name = UNICODE_STRING {
            Length: bytes,
            MaximumLength: bytes,
            Buffer: leaf.as_ptr().cast_mut(),
        };
        let object = OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: folder.as_raw_handle() as HANDLE,
            ObjectName: &name,
            Attributes: OBJ_CASE_INSENSITIVE,
            SecurityDescriptor: std::ptr::null(),
            SecurityQualityOfService: std::ptr::null(),
        };
        let mut handle: HANDLE = std::ptr::null_mut();
        let mut status_block = IO_STATUS_BLOCK::default();
        // SAFETY: every pointer argument is valid for the call, `leaf` outlives it, and the
        // folder handle stays open for as long as `self`.
        let status = unsafe {
            NtCreateFile(
                &mut handle,
                FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES,
                &object,
                &mut status_block,
                std::ptr::null(),
                if attributes == 0 {
                    FILE_ATTRIBUTE_NORMAL
                } else {
                    attributes
                },
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_CREATE,
                FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT,
                std::ptr::null(),
                0,
            )
        };
        Some(if status < 0 {
            if !handle.is_null() {
                // SAFETY: a handle returned with a failing status is still ours to close.
                unsafe { CloseHandle(handle) };
            }
            // SAFETY: RtlNtStatusToDosError only maps a status code.
            Err(io::Error::from_raw_os_error(
                unsafe { RtlNtStatusToDosError(status) } as i32,
            ))
        } else {
            // SAFETY: a successful NtCreateFile returns a new handle that nothing else owns.
            Ok(unsafe { File::from_raw_handle(handle as _) })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn creates_writes_and_refuses_an_existing_name_in_any_case() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("sub");
        std::fs::create_dir(&folder).unwrap();
        let folders = Folders::open([folder.clone(), folder.clone()]).unwrap();
        let path = folder.join("a b.txt");
        let mut file = folders.create_new(&path, 0).unwrap().unwrap();
        file.write_all(b"hi").unwrap();
        drop(file);
        assert_eq!(std::fs::read(&path).unwrap(), b"hi");
        let again = folders
            .create_new(&folder.join("A B.TXT"), 0)
            .unwrap()
            .unwrap_err();
        assert_eq!(again.kind(), io::ErrorKind::AlreadyExists);
        assert!(folders.create_new(&root.path().join("x"), 0).is_none());
    }

    #[test]
    fn applies_read_only_and_hidden_attributes() {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_READONLY,
        };
        let root = tempfile::tempdir().unwrap();
        let folders = Folders::open([root.path().to_owned()]).unwrap();
        let path = root.path().join("locked");
        drop(
            folders
                .create_new(&path, FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_HIDDEN)
                .unwrap()
                .unwrap(),
        );
        let metadata = std::fs::metadata(&path).unwrap();
        assert!(metadata.permissions().readonly());
        assert_ne!(
            std::os::windows::fs::MetadataExt::file_attributes(&metadata) & FILE_ATTRIBUTE_HIDDEN,
            0
        );
        let mut permissions = metadata.permissions();
        #[expect(
            clippy::permissions_set_readonly_false,
            reason = "Windows only, where it clears FILE_ATTRIBUTE_READONLY"
        )]
        permissions.set_readonly(false);
        std::fs::set_permissions(&path, permissions).unwrap();
    }
}
