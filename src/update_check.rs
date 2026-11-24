//! "Is there a newer release?"
//!
//! One unauthenticated GET to `https://github.com/finnuen/CopyFilePath/releases/latest`,
//! with redirects disabled: GitHub answers `302` with
//! `Location: .../releases/tag/<tag>`, so the tag is the last segment of that header.
//! No JSON parser, no certificate bundle, no rate limit.
//!
//! The request carries nothing about the user or the machine. A failure is not the
//! user's problem: `check()` reports it, `main.rs` decides whether to say anything,
//! and the launch check says nothing at all.

use std::{ffi::c_void, ptr};

use windows_sys::Win32::{
    Foundation::GetLastError,
    Networking::WinHttp::{
        WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
        WinHttpQueryHeaders, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption,
        WinHttpSetTimeouts, INTERNET_DEFAULT_HTTPS_PORT, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
        WINHTTP_DISABLE_REDIRECTS, WINHTTP_FLAG_SECURE, WINHTTP_OPTION_DISABLE_FEATURE,
        WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_LOCATION, WINHTTP_QUERY_STATUS_CODE,
    },
};

use crate::{log, version};

pub const RELEASE_PAGE: &str = "https://github.com/finnuen/CopyFilePath/releases/latest";

const HOST: &str = "github.com";
const PATH: &str = "/finnuen/CopyFilePath/releases/latest";
/// Per step, not in total: name lookup, connect, send, receive.
const TIMEOUT_MS: i32 = 1800;

pub enum Answer {
    /// A release newer than this build; the tag is what the user should read.
    Newer(String),
    UpToDate,
    /// GitHub has no release for the project yet, or answered without a redirect.
    NoRelease,
    /// The check did not happen (no network, blocked, GitHub down). Never shown on
    /// a launch; shown when the user asked for the check.
    Unknown(String),
}

pub fn check() -> Answer {
    let current = version::short(env!("CARGO_PKG_VERSION"));
    match fetch_tag() {
        Ok(Some(tag)) => {
            if version::is_newer(&tag, &current) {
                log::write(&format!("update check: {tag} is newer than {current}"));
                Answer::Newer(tag)
            } else {
                log::write(&format!("update check: {tag} is not newer than {current}"));
                Answer::UpToDate
            }
        }
        Ok(None) => {
            log::write("update check: no release on GitHub");
            Answer::NoRelease
        }
        Err(detail) => {
            log::write(&format!("update check failed: {detail}"));
            Answer::Unknown(detail)
        }
    }
}

/// `Ok(Some(tag))` when GitHub named a release, `Ok(None)` when it has none.
fn fetch_tag() -> Result<Option<String>, String> {
    let agent: Vec<u16> = format!("CopyFilePath/{}", env!("CARGO_PKG_VERSION"))
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let session = Handle::new(
        unsafe {
            WinHttpOpen(
                agent.as_ptr(),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                ptr::null(),
                ptr::null(),
                0,
            )
        },
        "a session",
    )?;
    unsafe {
        WinHttpSetTimeouts(session.0, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS);
    }

    let host: Vec<u16> = HOST
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let connection = Handle::new(
        unsafe { WinHttpConnect(session.0, host.as_ptr(), INTERNET_DEFAULT_HTTPS_PORT, 0) },
        "a connection",
    )?;

    let verb: Vec<u16> = "GET".encode_utf16().chain(std::iter::once(0)).collect();
    let path: Vec<u16> = PATH.encode_utf16().chain(std::iter::once(0)).collect();
    let request = Handle::new(
        unsafe {
            WinHttpOpenRequest(
                connection.0,
                verb.as_ptr(),
                path.as_ptr(),
                ptr::null(),
                ptr::null(),
                ptr::null(),
                WINHTTP_FLAG_SECURE,
            )
        },
        "a request",
    )?;

    // The redirect is the answer, so do not follow it.
    let disable: u32 = WINHTTP_DISABLE_REDIRECTS;
    unsafe {
        WinHttpSetOption(
            request.0,
            WINHTTP_OPTION_DISABLE_FEATURE,
            &disable as *const u32 as *const c_void,
            std::mem::size_of::<u32>() as u32,
        );
        if WinHttpSendRequest(request.0, ptr::null(), 0, ptr::null(), 0, 0, 0) == 0 {
            return Err(format!("send failed (error {})", last_error()));
        }
        if WinHttpReceiveResponse(request.0, ptr::null_mut()) == 0 {
            return Err(format!("no answer (error {})", last_error()));
        }
    }

    let status = query_number(&request, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER)?;
    if status == 404 {
        return Ok(None);
    }
    if !(300..400).contains(&status) {
        return Err(format!("GitHub answered {status}"));
    }

    let location =
        query_text(&request, WINHTTP_QUERY_LOCATION).map_err(|e| format!("no Location header ({e})"))?;
    match tag_from(&location) {
        Some(tag) => Ok(Some(tag)),
        // A project with no release yet redirects to /releases, not to a tag.
        None if location.trim_end_matches('/').ends_with("/releases") => Ok(None),
        None => Err(format!("unexpected redirect: {location}")),
    }
}

/// `.../releases/tag/1.2.3` (or `/tag/v1.2.3`) -> `1.2.3`, `v1.2.3`.
fn tag_from(location: &str) -> Option<String> {
    let location = location.split(['?', '#']).next().unwrap_or(location);
    let (_, tag) = location.rsplit_once("/releases/tag/")?;
    let tag = tag.trim_end_matches('/');
    (!tag.is_empty()).then(|| tag.to_string())
}

// ---------------------------------------------------------------- WinHTTP helpers

/// Closes its handle when it goes out of scope, so every early return is clean.
struct Handle(*mut c_void);

impl Handle {
    fn new(raw: *mut c_void, what: &str) -> Result<Self, String> {
        if raw.is_null() {
            Err(format!("could not open {what} (error {})", last_error()))
        } else {
            Ok(Self(raw))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

fn last_error() -> u32 {
    unsafe { GetLastError() }
}

fn query_number(request: &Handle, info: u32) -> Result<u32, String> {
    let mut value: u32 = 0;
    let mut len = std::mem::size_of::<u32>() as u32;
    let ok = unsafe {
        WinHttpQueryHeaders(
            request.0,
            info,
            ptr::null(),
            &mut value as *mut u32 as *mut c_void,
            &mut len,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(format!("status unavailable (error {})", last_error()))
    } else {
        Ok(value)
    }
}

fn query_text(request: &Handle, info: u32) -> Result<String, String> {
    // Ask for the size first, then for the value.
    let mut len: u32 = 0;
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            info,
            ptr::null(),
            ptr::null_mut(),
            &mut len,
            ptr::null_mut(),
        )
    };
    if len == 0 {
        return Err(format!("header unavailable (error {})", last_error()));
    }

    let mut buffer = vec![0u16; len as usize / 2 + 2];
    let mut len = (buffer.len() * 2) as u32;
    let ok = unsafe {
        WinHttpQueryHeaders(
            request.0,
            info,
            ptr::null(),
            buffer.as_mut_ptr() as *mut c_void,
            &mut len,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(format!("header unavailable (error {})", last_error()));
    }

    let used = buffer.iter().position(|unit| *unit == 0).unwrap_or(buffer.len());
    Ok(String::from_utf16_lossy(&buffer[..used]))
}

#[cfg(test)]
mod tests {
    use super::tag_from;

    #[test]
    fn the_tag_is_the_last_segment_of_a_release_redirect() {
        assert_eq!(
            tag_from("https://github.com/finnuen/CopyFilePath/releases/tag/1.2.3"),
            Some("1.2.3".to_string())
        );
    }

    #[test]
    fn a_trailing_slash_and_a_query_are_ignored() {
        assert_eq!(
            tag_from("https://github.com/finnuen/CopyFilePath/releases/tag/v1.2.3/?x=1"),
            Some("v1.2.3".to_string())
        );
    }

    #[test]
    fn something_that_is_not_a_release_redirect_has_no_tag() {
        assert_eq!(tag_from("https://github.com/finnuen/CopyFilePath/releases"), None);
    }
}
