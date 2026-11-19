//! Daemon-managed, route-scoped runtime credential for the CLI usage report.
//!
//! This secret is intentionally separate from the regular client-key store.
//! It is generated on every daemon start, stored only in a private runtime
//! file, and removed on graceful shutdown when the file still contains this
//! daemon's token.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng;

const TOKEN_BYTES: usize = 32;
const TOKEN_CHARS: usize = 43;
const TEMP_ATTEMPTS: usize = 8;

/// A runtime usage secret and its cleanup lease.
pub(crate) struct UsageKey {
    path: PathBuf,
    token: String,
}

impl UsageKey {
    /// Generate and atomically persist a fresh daemon token.
    pub(crate) fn create(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        protect_path(parent, true)?;

        let token = generate_token();
        write_private_atomic(&path, token.as_bytes())?;
        Ok(Self { path, token })
    }

    /// The exact bearer token held by the daemon.
    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    /// Read a bounded, correctly shaped token without including its contents in errors.
    pub(crate) fn read(path: &Path) -> io::Result<String> {
        let file = File::open(path)?;
        let metadata = file.metadata()?;
        if metadata.len() != TOKEN_CHARS as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid usage credential file",
            ));
        }

        let mut bytes = Vec::with_capacity(TOKEN_CHARS);
        file.take((TOKEN_CHARS + 1) as u64)
            .read_to_end(&mut bytes)?;
        if !valid_token_bytes(&bytes) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid usage credential file",
            ));
        }
        String::from_utf8(bytes).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid usage credential file")
        })
    }
}

impl Drop for UsageKey {
    fn drop(&mut self) {
        let Ok(file) = File::open(&self.path) else {
            return;
        };
        let Ok(metadata) = file.metadata() else {
            return;
        };
        if metadata.len() != self.token.len() as u64 {
            return;
        }
        let mut bytes = Vec::with_capacity(self.token.len());
        if file
            .take((self.token.len() + 1) as u64)
            .read_to_end(&mut bytes)
            .is_err()
        {
            return;
        }
        if crate::keys::constant_time_eq(&bytes, self.token.as_bytes()) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn generate_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn valid_token_bytes(bytes: &[u8]) -> bool {
    bytes.len() == TOKEN_CHARS
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-' || *byte == b'_')
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage credential path has no file name",
        )
    })?;

    for _ in 0..TEMP_ATTEMPTS {
        let temporary = parent.join(format!(
            ".{}.tmp.{}.{}",
            file_name.to_string_lossy(),
            std::process::id(),
            random_component(),
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        let mut file = match options.open(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };

        let result = (|| {
            // On Windows this installs a protected, current-user-only DACL
            // before any token bytes are written. Unix uses mode 0600 above.
            protect_path(&temporary, false)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            replace_file(&temporary, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
            return result;
        }

        // Persist the directory entry when the platform supports syncing it.
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        return Ok(());
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique usage credential temporary file",
    ))
}

fn random_component() -> String {
    let mut bytes = [0u8; 16];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(unix)]
fn protect_path(path: &Path, directory: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
    )
}

#[cfg(windows)]
fn protect_path(path: &Path, directory: bool) -> io::Result<()> {
    use std::ffi::OsStr;
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, EqualSid, GetTokenInformation,
        PROTECTED_DACL_SECURITY_INFORMATION, SetFileSecurityW, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // SAFETY: the process pseudo-handle is valid and the output handle is writable.
    let mut token_handle = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_handle) } == 0 {
        return Err(io::Error::last_os_error());
    }
    struct TokenHandle(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for TokenHandle {
        fn drop(&mut self) {
            // SAFETY: this wrapper owns the handle returned by OpenProcessToken.
            unsafe { CloseHandle(self.0) };
        }
    }
    let token_handle = TokenHandle(token_handle);

    let mut needed = 0u32;
    // SAFETY: this is the documented size query; no output buffer is supplied.
    unsafe {
        GetTokenInformation(token_handle.0, TokenUser, null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut token_user = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    // SAFETY: the aligned buffer is sized from the preceding TokenUser query.
    if unsafe {
        GetTokenInformation(
            token_handle.0,
            TokenUser,
            token_user.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetTokenInformation filled a correctly aligned TOKEN_USER value.
    let user = unsafe { &*token_user.as_ptr().cast::<TOKEN_USER>() };
    let mut sid_text = null_mut();
    // SAFETY: the SID came from the current process token and sid_text is writable.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let sid = unsafe {
        let mut length = 0usize;
        while *sid_text.add(length) != 0 {
            length += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(sid_text, length))
    };
    // SAFETY: sid_text was allocated by ConvertSidToStringSidW.
    unsafe { LocalFree(sid_text.cast()) };

    let inheritance = if directory { "OICI" } else { "" };
    let descriptor_text: Vec<u16> = OsStr::new(&format!("D:P(A;{inheritance};FA;;;{sid})"))
        .encode_wide()
        .chain(once(0))
        .collect();
    let mut descriptor = null_mut();
    // SAFETY: descriptor_text is nul-terminated, descriptor is writable, and
    // the descriptor is freed below with LocalFree.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor_text.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }

    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(once(0)).collect();
    // SAFETY: both pointers remain valid for the call; the descriptor contains
    // a protected DACL granting full access only to the current token's user SID.
    let result = unsafe {
        SetFileSecurityW(
            path_wide.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };
    let error = (result == 0).then(io::Error::last_os_error);
    // SAFETY: descriptor was allocated by the Windows SDDL conversion API.
    unsafe { LocalFree(descriptor.cast()) };
    error.map_or(Ok(()), Err)
}

#[cfg(not(any(unix, windows)))]
fn protect_path(_path: &Path, _directory: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "private usage credential permissions are unsupported on this platform",
    ))
}

#[cfg(unix)]
fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    use std::iter::once;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let from_wide: Vec<u16> = from.as_os_str().encode_wide().chain(once(0)).collect();
    let to_wide: Vec<u16> = to.as_os_str().encode_wide().chain(once(0)).collect();
    // SAFETY: both paths are valid nul-terminated UTF-16 strings. The replace
    // flag gives rotation atomic replacement semantics on Windows.
    if unsafe {
        MoveFileExW(
            from_wide.as_ptr(),
            to_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_replaces_the_token_and_old_cleanup_cannot_remove_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.key");
        let first = UsageKey::create(&path).unwrap();
        let first_token = first.token().to_owned();
        assert!(valid_token_bytes(first_token.as_bytes()));
        assert!(crate::keys::constant_time_eq(
            UsageKey::read(&path).unwrap().as_bytes(),
            first_token.as_bytes()
        ));

        let second = UsageKey::create(&path).unwrap();
        let second_token = second.token().to_owned();
        assert!(
            !crate::keys::constant_time_eq(first_token.as_bytes(), second_token.as_bytes()),
            "daemon starts rotate the usage credential"
        );

        drop(first);
        assert!(crate::keys::constant_time_eq(
            UsageKey::read(&path).unwrap().as_bytes(),
            second_token.as_bytes()
        ));
        drop(second);
        assert!(!path.exists(), "matching graceful cleanup removes the file");
    }

    #[test]
    fn a_failed_private_file_creation_is_reported() {
        let directory = tempfile::tempdir().unwrap();
        let not_a_directory = directory.path().join("file");
        fs::write(&not_a_directory, "not a directory").unwrap();
        let error = UsageKey::create(not_a_directory.join("usage.key"))
            .err()
            .expect("creation under a file fails");
        assert_ne!(error.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn the_directory_and_secret_file_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("eggpass");
        let path = data.join("usage.key");
        let key = UsageKey::create(&path).unwrap();
        let dir_mode = fs::metadata(&data).unwrap().permissions().mode() & 0o777;
        let file_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        assert_eq!(file_mode, 0o600);
        drop(key);
    }

    #[cfg(windows)]
    #[test]
    fn the_directory_and_secret_file_have_current_user_only_dacls() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("eggpass");
        let path = data.join("usage.key");
        let key = UsageKey::create(&path).unwrap();
        assert!(windows_acl_is_current_user_only(&data, true));
        assert!(windows_acl_is_current_user_only(&path, false));
        drop(key);
    }

    #[cfg(windows)]
    fn windows_acl_is_current_user_only(path: &Path, directory: bool) -> bool {
        use std::iter::once;
        use std::os::windows::ffi::OsStrExt;
        use std::ptr::null_mut;
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::Security::{
            ACCESS_ALLOWED_ACE, ACCESS_ALLOWED_ACE_TYPE, ACL_SIZE_INFORMATION, AclSizeInformation,
            CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation,
            GetFileSecurityW, GetSecurityDescriptorDacl, GetTokenInformation, OBJECT_INHERIT_ACE,
            TOKEN_QUERY, TOKEN_USER, TokenUser,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(once(0)).collect();
        let mut required = 0u32;
        // SAFETY: size query for the DACL security descriptor.
        unsafe {
            GetFileSecurityW(
                path_wide.as_ptr(),
                DACL_SECURITY_INFORMATION,
                null_mut(),
                0,
                &mut required,
            )
        };
        if required == 0 {
            return false;
        }
        let mut descriptor =
            vec![0usize; (required as usize).div_ceil(std::mem::size_of::<usize>())];
        // SAFETY: the aligned descriptor buffer has the required size.
        if unsafe {
            GetFileSecurityW(
                path_wide.as_ptr(),
                DACL_SECURITY_INFORMATION,
                descriptor.as_mut_ptr().cast(),
                required,
                &mut required,
            )
        } == 0
        {
            return false;
        }
        let mut dacl_present = 0;
        let mut dacl = null_mut();
        let mut dacl_defaulted = 0;
        // SAFETY: descriptor contains the DACL requested above.
        if unsafe {
            GetSecurityDescriptorDacl(
                descriptor.as_mut_ptr().cast(),
                &mut dacl_present,
                &mut dacl,
                &mut dacl_defaulted,
            )
        } == 0
            || dacl_present == 0
            || dacl.is_null()
        {
            return false;
        }
        let mut acl_info = ACL_SIZE_INFORMATION {
            AceCount: 0,
            AclBytesInUse: 0,
            AclBytesFree: 0,
        };
        // SAFETY: dacl is valid and acl_info is writable.
        if unsafe {
            GetAclInformation(
                dacl,
                (&mut acl_info as *mut ACL_SIZE_INFORMATION).cast(),
                std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        } == 0
            || acl_info.AceCount != 1
        {
            return false;
        }
        let mut ace_ptr = null_mut();
        // SAFETY: index 0 exists because AceCount is one.
        if unsafe { GetAce(dacl, 0, &mut ace_ptr) } == 0 || ace_ptr.is_null() {
            return false;
        }
        let ace = unsafe { &*ace_ptr.cast::<ACCESS_ALLOWED_ACE>() };
        let ace_sid = (&ace.SidStart as *const u32).cast_mut().cast();

        let mut token = null_mut();
        // SAFETY: the process pseudo-handle is valid and token output is writable.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return false;
        }
        let mut needed = 0u32;
        // SAFETY: size query for TokenUser.
        unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed) };
        if needed == 0 {
            unsafe { CloseHandle(token) };
            return false;
        }
        let mut user_buffer =
            vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
        // SAFETY: the aligned buffer has the queried size.
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                user_buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            unsafe { CloseHandle(token) };
            return false;
        }
        let user = unsafe { &*user_buffer.as_ptr().cast::<TOKEN_USER>() };
        let same_user = unsafe { EqualSid(ace_sid, user.User.Sid) } != 0;
        unsafe { CloseHandle(token) };
        let expected_flags = if directory {
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
        } else {
            0
        };
        same_user
            && ace.Header.AceType == ACCESS_ALLOWED_ACE_TYPE
            && ace.Header.AceFlags == expected_flags
    }

    #[test]
    fn invalid_usage_file_is_rejected_without_echoing_contents() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.key");
        fs::write(&path, "not-a-token").unwrap();
        let error = UsageKey::read(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "invalid usage credential file");
    }
}
