use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io;
use std::os::windows::io::{AsRawHandle, IntoRawHandle, RawHandle};
use std::path::Path;

// For correctness, it is critical that both file handles remain open while
// their attributes are checked for equality. In particular, the file index
// numbers on a Windows stat object are not guaranteed to remain stable over
// time.
//
// See the docs and remarks on MSDN:
// https://msdn.microsoft.com/en-us/library/windows/desktop/aa363788(v=vs.85).aspx
//
// It gets worse. It appears that the index numbers are not always
// guaranteed to be unique. Namely, ReFS uses 128 bit numbers for unique
// identifiers. This requires a distinct syscall to get `FILE_ID_INFO`
// documented here:
// https://msdn.microsoft.com/en-us/library/windows/desktop/hh802691(v=vs.85).aspx
//
// It seems straight-forward enough to modify this code to use
// `FILE_ID_INFO` when available (minimum Windows Server 2012), but I don't
// have access to such Windows machines.
//
// Two notes.
//
// 1. Java's NIO uses the approach implemented here and appears to ignore
//    `FILE_ID_INFO` altogether. So Java's NIO and this code are
//    susceptible to bugs when running on a file system where
//    `nFileIndex{Low,High}` are not unique.
//
// 2. LLVM has a bug where they fetch the id of a file and continue to use
//    it even after the handle has been closed, so that uniqueness is no
//    longer guaranteed (when `nFileIndex{Low,High}` are unique).
//    bug report: http://lists.llvm.org/pipermail/llvm-bugs/2014-December/037218.html
//
// All said and done, checking whether two files are the same on Windows
// seems quite tricky. Moreover, even if the code is technically incorrect,
// it seems like the chances of actually observing incorrect behavior are
// extremely small. Nevertheless, we mitigate this by checking size too.
//
// In the case where this code is erroneous, two files will be reported
// as equivalent when they are in fact distinct. This will cause the loop
// detection code to report a false positive, which will prevent descending
// into the offending directory. As far as failure modes goes, this isn't
// that bad.

#[derive(Debug)]
pub struct Handle {
    kind: HandleKind,
    key: Option<Key>,
}

#[derive(Debug)]
enum HandleKind {
    /// Used when opening a file or acquiring ownership of a file.
    Owned(winutil::Handle),
    /// Used for stdio.
    Borrowed(winutil::HandleRef),
}

#[derive(Debug, Eq, PartialEq, Hash)]
struct Key {
    volume: u64,
    index: u64,
}

impl Eq for Handle {}

impl PartialEq for Handle {
    fn eq(&self, other: &Handle) -> bool {
        // Need this branch to satisfy `Eq` since `Handle`s with
        // `key.is_none()` wouldn't otherwise.
        if std::ptr::eq(self as *const Handle, other as *const Handle) {
            return true;
        } else if self.key.is_none() || other.key.is_none() {
            return false;
        }
        self.key == other.key
    }
}

impl AsRawHandle for crate::Handle {
    fn as_raw_handle(&self) -> RawHandle {
        match self.0.kind {
            HandleKind::Owned(ref h) => h.as_raw_handle(),
            HandleKind::Borrowed(ref h) => h.as_raw_handle(),
        }
    }
}

impl IntoRawHandle for crate::Handle {
    fn into_raw_handle(self) -> RawHandle {
        match self.0.kind {
            HandleKind::Owned(h) => h.into_raw_handle(),
            HandleKind::Borrowed(h) => h.into_raw_handle(),
        }
    }
}

impl Hash for Handle {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key.hash(state);
    }
}

impl Handle {
    pub fn from_path<P: AsRef<Path>>(p: P) -> io::Result<Handle> {
        Self::from_handle(winutil::Handle::from_path(p)?)
    }

    pub fn from_file(file: File) -> io::Result<Handle> {
        Self::from_handle(winutil::Handle::from_file(file))
    }

    fn from_handle(handle: winutil::Handle) -> io::Result<Handle> {
        let key = winutil::information(&handle)?;
        Ok(Handle { kind: HandleKind::Owned(handle), key: Some(key) })
    }

    fn from_std_handle(h: winutil::HandleRef) -> io::Result<Handle> {
        match winutil::information(&h) {
            Ok(key) => Ok(Handle { kind: HandleKind::Borrowed(h), key: Some(key) }),
            // In a Windows console, if there is no pipe attached to a STD
            // handle, then GetFileInformationByHandle will return an error.
            // We don't really care. The only thing we care about is that
            // this handle is never equivalent to any other handle, which is
            // accomplished by setting key to None.
            Err(_) => Ok(Handle { kind: HandleKind::Borrowed(h), key: None }),
        }
    }

    pub fn stdin() -> io::Result<Handle> {
        Handle::from_std_handle(winutil::HandleRef::stdin())
    }

    pub fn stdout() -> io::Result<Handle> {
        Handle::from_std_handle(winutil::HandleRef::stdout())
    }

    pub fn stderr() -> io::Result<Handle> {
        Handle::from_std_handle(winutil::HandleRef::stderr())
    }

    pub fn as_file(&self) -> &File {
        match self.kind {
            HandleKind::Owned(ref h) => h.as_file(),
            HandleKind::Borrowed(ref h) => h.as_file(),
        }
    }

    pub fn as_file_mut(&mut self) -> &mut File {
        match self.kind {
            HandleKind::Owned(ref mut h) => h.as_file_mut(),
            HandleKind::Borrowed(ref mut h) => h.as_file_mut(),
        }
    }
}

mod winutil {
    use super::Key;
    use std::fs::File;
    use std::io;
    use std::os::windows::io::{
        AsRawHandle, FromRawHandle, IntoRawHandle, RawHandle,
    };
    use std::path::Path;
    use windows_sys::Win32::Storage::FileSystem as winfs;

    #[derive(Debug)]
    pub(super) struct Handle(File);

    impl Handle {
        pub fn from_file(file: File) -> Self {
            Self(file)
        }

        pub fn from_path<P: AsRef<Path>>(path: P) -> io::Result<Self> {
            use std::fs::OpenOptions;
            use std::os::windows::fs::OpenOptionsExt;
            use winfs::FILE_FLAG_BACKUP_SEMANTICS;

            let file = OpenOptions::new()
                .read(true)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(path)?;

            Ok(Self(file))
        }

        pub fn as_file(&self) -> &File {
            &self.0
        }

        pub fn as_file_mut(&mut self) -> &mut File {
            &mut self.0
        }
    }

    impl AsRawHandle for &Handle {
        fn as_raw_handle(&self) -> RawHandle {
            self.0.as_raw_handle()
        }
    }

    impl IntoRawHandle for Handle {
        fn into_raw_handle(self) -> RawHandle {
            self.0.into_raw_handle()
        }
    }

    #[derive(Debug)]
    pub(super) struct HandleRef(Option<File>);

    impl Drop for HandleRef {
        fn drop(&mut self) {
            self.0.take().unwrap().into_raw_handle();
        }
    }

    impl HandleRef {
        pub fn from_raw_handle(handle: RawHandle) -> Self {
            unsafe { Self(Some(File::from_raw_handle(handle))) }
        }

        pub fn stdin() -> Self {
            Self::from_raw_handle(io::stdin().as_raw_handle())
        }

        pub fn stdout() -> Self {
            Self::from_raw_handle(io::stdout().as_raw_handle())
        }

        pub fn stderr() -> Self {
            Self::from_raw_handle(io::stderr().as_raw_handle())
        }

        pub fn as_file(&self) -> &File {
            self.0.as_ref().unwrap()
        }

        pub fn as_file_mut(&mut self) -> &mut File {
            self.0.as_mut().unwrap()
        }
    }

    impl AsRawHandle for &HandleRef {
        fn as_raw_handle(&self) -> RawHandle {
            self.as_file().as_raw_handle()
        }
    }

    impl IntoRawHandle for HandleRef {
        fn into_raw_handle(mut self) -> RawHandle {
            self.0.take().unwrap().into_raw_handle()
        }
    }

    pub(super) fn information<H: AsRawHandle>(handle: H) -> io::Result<Key> {
        use winfs::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION};
        unsafe {
            let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            match GetFileInformationByHandle(handle.as_raw_handle() as isize, &mut info) {
                0 => Err(io::Error::last_os_error()),
                _ => Ok(Key {
                    volume: info.dwVolumeSerialNumber as u64,
                    index: ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64),
                })
            }
        }
    }
}
