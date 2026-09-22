//! The Win32 half: creating the sections, and copying when somebody beat us
//! to one.
//!
//! This is the only module with `unsafe` in it, and the only one that cannot
//! be tested on a Linux machine. Everything it decides is decided in
//! [`crate::shadow`] and [`crate::pacing`], which can.

use crate::announce::Mode;
use crate::page::Page;
use crate::store::Store;
use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    OpenFileMappingW, PAGE_READWRITE, UnmapViewOfFile,
};
use windows::core::HSTRING;

/// A mapped view of a section, unmapped when dropped.
struct View {
    at: MEMORY_MAPPED_VIEW_ADDRESS,
    bytes: usize,
}

impl View {
    /// The bytes, read only.
    fn as_slice(&self) -> &[u8] {
        // SAFETY: `at` came from `MapViewOfFile` for a section of at least
        // `bytes`, and the view lives as long as `self`.
        unsafe { std::slice::from_raw_parts(self.at.Value as *const u8, self.bytes) }
    }

    /// The bytes, writable. Only ever called on a view this process owns.
    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above, and this view was mapped `FILE_MAP_WRITE` from a
        // section backed by a file nothing else is writing through.
        unsafe { std::slice::from_raw_parts_mut(self.at.Value as *mut u8, self.bytes) }
    }
}

impl Drop for View {
    fn drop(&mut self) {
        // SAFETY: `at` was produced by `MapViewOfFile` and is unmapped once.
        let _ = unsafe { UnmapViewOfFile(self.at) };
    }
}

/// A handle that closes itself.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: produced by a Win32 call that returned success, closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// One page, published.
pub struct Published {
    page: Page,
    mode: Mode,
    /// Owned: the section, held open so the name stays taken. Mirrored: the
    /// writer's section, read from.
    _section: Owned,
    /// Mirrored only: the writer's bytes, and our file's bytes.
    copying: Option<Copying>,
    shadow: crate::Shadow,
    pacing: crate::Pacing,
}

struct Copying {
    from: View,
    into: View,
    /// Held so the destination mapping stays valid.
    _into_section: Owned,
}

impl Published {
    /// Which page this is.
    pub fn page(&self) -> &Page {
        &self.page
    }

    /// How it is being served.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// What has been copied, for the summary.
    pub fn shadow(&self) -> &crate::Shadow {
        &self.shadow
    }

    /// How often it is currently being looked at.
    pub fn pacing(&self) -> &crate::Pacing {
        &self.pacing
    }

    /// Publish one page: create the section, or open the one already there.
    ///
    /// **The whole design is in the `ERROR_ALREADY_EXISTS` branch.**
    /// `CreateFileMappingW` given a name nobody holds makes a section backed
    /// by the file we hand it, and from then on the writer's stores land in
    /// `/dev/shm` with nothing copying anything. Given a name somebody
    /// already holds, it hands back *their* section and silently ignores our
    /// file — so a bridge started second would create nothing, publish a file
    /// frozen at whatever was in it, and look like it was working.
    pub fn open(store: &Store, page: Page, pacing: crate::Pacing) -> io::Result<Self> {
        let file = store.prepare(&page)?;
        let name = HSTRING::from(page.name.as_str());
        let (high, low) = split_size(page.bytes);

        // SAFETY: the file handle outlives the call, and the name is valid
        // for its duration. The returned handle is owned by `Owned`.
        let made = unsafe {
            CreateFileMappingW(
                HANDLE(file.as_raw_handle() as _),
                None,
                PAGE_READWRITE,
                high,
                low,
                &name,
            )
        };
        let handle = made.map_err(|why| io::Error::other(format!("{}: {why}", page.name)))?;
        // SAFETY: reading the calling thread's own last-error value, set by
        // the call immediately above and read before anything else can
        // overwrite it.
        let already = unsafe { windows::Win32::Foundation::GetLastError() } == ERROR_ALREADY_EXISTS;
        let section = Owned(handle);

        if !already {
            return Ok(Self {
                page,
                mode: Mode::Owned,
                _section: section,
                copying: None,
                shadow: crate::Shadow::new(0),
                pacing,
            });
        }

        // Somebody was here first. Their section is the one that matters, so
        // ours is dropped and theirs is opened for reading.
        drop(section);
        let copying = Copying::open(&page, &file)?;
        Ok(Self {
            shadow: crate::Shadow::new(page.bytes),
            page,
            mode: Mode::Mirrored,
            _section: copying.1,
            copying: Some(copying.0),
            pacing,
        })
    }

    /// One look. Returns how long to wait before the next one.
    ///
    /// An owned page never gets here — there is nothing to look at.
    pub fn tick(&mut self) -> core::time::Duration {
        let Some(copying) = self.copying.as_mut() else {
            return self.pacing.interval();
        };

        let fresh = copying.from.as_slice();
        let changed = self.shadow.changed(fresh);
        if changed {
            // **A memcpy, and no syscall.** The destination is our own file
            // mapped into this process, so publishing is a store to memory
            // that the kernel writes back. The bridge this replaces did a
            // `seek` and a `write` per page per tick, which is two syscalls
            // two hundred and fifty times a second for pages that mostly had
            // not changed.
            let into = copying.into.as_mut_slice();
            let take = fresh.len().min(into.len());
            into[..take].copy_from_slice(&fresh[..take]);
        }
        self.pacing.after(changed)
    }
}

impl Copying {
    fn open(page: &Page, file: &File) -> io::Result<(Self, Owned)> {
        let name = HSTRING::from(page.name.as_str());
        // SAFETY: the name outlives the call; the handle is owned below.
        let theirs = unsafe { OpenFileMappingW(FILE_MAP_READ.0, false, &name) }
            .map_err(|why| io::Error::other(format!("{}: {why}", page.name)))?;
        let theirs = Owned(theirs);

        // SAFETY: a valid section handle, mapped for the whole section.
        let from = unsafe { MapViewOfFile(theirs.0, FILE_MAP_READ, 0, 0, page.bytes) };
        if from.Value.is_null() {
            return Err(io::Error::other(format!(
                "{}: the writer's section could not be viewed",
                page.name
            )));
        }

        // Our own file, mapped so that publishing is a memcpy. Unnamed: this
        // one is for us, and a second name would collide with the writer's.
        let (high, low) = split_size(page.bytes);
        // SAFETY: our own file handle, valid for the call.
        let ours = unsafe {
            CreateFileMappingW(
                HANDLE(file.as_raw_handle() as _),
                None,
                PAGE_READWRITE,
                high,
                low,
                None,
            )
        }
        .map_err(|why| io::Error::other(format!("{}: {why}", page.name)))?;
        let ours = Owned(ours);

        // SAFETY: as above, mapped writable for the whole file.
        let into = unsafe { MapViewOfFile(ours.0, FILE_MAP_WRITE, 0, 0, page.bytes) };
        if into.Value.is_null() {
            return Err(io::Error::other(format!(
                "{}: our own file could not be mapped",
                page.name
            )));
        }

        Ok((
            Self {
                from: View {
                    at: from,
                    bytes: page.bytes,
                },
                into: View {
                    at: into,
                    bytes: page.bytes,
                },
                _into_section: ours,
            },
            theirs,
        ))
    }
}

/// A size, split the way `CreateFileMappingW` wants it.
fn split_size(bytes: usize) -> (u32, u32) {
    let bytes = bytes as u64;
    (
        ((bytes >> 32) & 0xFFFF_FFFF) as u32,
        (bytes & 0xFFFF_FFFF) as u32,
    )
}

/// Whether a person is on the other end of this program's input.
///
/// **The program stops when its input ends, and that is right twice and wrong
/// once.** A parent process closing a pipe is asking it to stop; somebody in
/// a terminal pressing Ctrl-D is asking it to stop. Started from a file
/// manager or a desktop entry there is no input at all, the first read ends
/// immediately, and the program would unlink everything it had just published
/// and exit inside a second — which from the outside is "running it does
/// nothing".
///
/// `GetFileType` cannot tell these apart under Wine: it answers
/// `FILE_TYPE_CHAR` for a console, a pipe and `/dev/null` alike.
/// `GetConsoleMode` succeeds only for a real console, which is the half that
/// can be answered without waiting; the rest is answered by how quickly the
/// input ends. See [`crate::cli`]'s caller.
pub fn stdin_is_a_console() -> bool {
    use windows::Win32::System::Console::{CONSOLE_MODE, GetConsoleMode};

    let handle = HANDLE(std::io::stdin().as_raw_handle() as _);
    if handle.is_invalid() {
        return false;
    }
    let mut mode = CONSOLE_MODE::default();
    // SAFETY: the handle is this process's own standard input and the call
    // does not consume it.
    unsafe { GetConsoleMode(handle, &mut mode) }.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_is_split_into_the_two_halves_win32_wants() {
        assert_eq!(split_size(0), (0, 0));
        assert_eq!(split_size(2048), (0, 2048));
        assert_eq!(split_size(0xFFFF_FFFF), (0, 0xFFFF_FFFF));
        assert_eq!(split_size(0x1_0000_0000), (1, 0));
        assert_eq!(split_size(0x1_2345_6789), (1, 0x2345_6789));
    }
}
