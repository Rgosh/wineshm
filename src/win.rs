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

impl View {
    /// Map a whole section this process holds a handle to.
    fn of(
        section: &Owned,
        bytes: usize,
        how: windows::Win32::System::Memory::FILE_MAP,
    ) -> io::Result<Self> {
        // SAFETY: a valid section handle, mapped for the whole section.
        let at = unsafe { MapViewOfFile(section.0, how, 0, 0, bytes) };
        if at.Value.is_null() {
            return Err(io::Error::other(
                "a section this process created could not be viewed",
            ));
        }
        Ok(Self { at, bytes })
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
    /// Owned only: a writable view of the page, which is the section itself.
    /// The mirrored case reaches the same bytes through `copying.into`.
    mine: Option<View>,
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
            // **A view of our own page, even though nothing is copied into
            // it.** An owned page needs no work to stay current — the writer
            // stores straight into it — but it still has to be *looked at*,
            // because the writer going away is what this program has no other
            // way to notice, and blanking it afterwards means writing to it.
            let mine = View::of(&section, page.bytes, FILE_MAP_WRITE)?;
            return Ok(Self {
                shadow: crate::Shadow::new(page.bytes),
                page,
                mode: Mode::Owned,
                _section: section,
                copying: None,
                mine: Some(mine),
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
            mine: None,
            pacing,
        })
    }

    /// Look at the page without copying anything, and say whether it moved.
    ///
    /// For the heartbeat: the block the caller named as the one that changes
    /// while the writer is alive. Works in both modes — an owned page is read
    /// through our own view of the section, a mirrored one through the
    /// writer's.
    pub fn stirred(&mut self) -> bool {
        let bytes = match (self.mine.as_ref(), self.copying.as_ref()) {
            (Some(mine), _) => mine.as_slice(),
            (None, Some(copying)) => copying.from.as_slice(),
            (None, None) => return false,
        };
        self.shadow.changed(bytes)
    }

    /// Zero the page.
    ///
    /// **Because the bytes outlive the program that wrote them.** A game
    /// exits, the section stays — this bridge is holding it — and the file
    /// goes on holding the last frame. Zeroes are the state every reader
    /// already waits through, and the state a page is in before anybody
    /// writes to it.
    pub fn blank(&mut self) {
        if let Some(mine) = self.mine.as_mut() {
            mine.as_mut_slice().fill(0);
        }
        if let Some(copying) = self.copying.as_mut() {
            copying.into.as_mut_slice().fill(0);
        }
        // So that the writer's first frame after this counts as a change
        // rather than as "the same as what I last saw".
        self.shadow = crate::Shadow::new(self.page.bytes);
    }

    /// One look. Returns how long to wait before the next one.
    ///
    /// An owned page never gets here — there is nothing to copy.
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

/// Look at a section somebody else published, without publishing anything.
///
/// **So that a size can be measured rather than guessed.** The number of bytes
/// in a page is a fact about a program somebody else wrote, and getting it
/// wrong fails quietly — see [`crate::probe`]. Mapping the section with a
/// length of zero maps the whole of it, whatever that is, and `VirtualQuery`
/// then says how much address space that took.
///
/// `Ok(None)` means no section of that name exists, which is an answer and not
/// a failure: it is what "the game is not running" looks like from here.
pub fn look_at(name: &str) -> io::Result<Option<crate::probe::Found>> {
    use windows::Win32::System::Memory::{MEMORY_BASIC_INFORMATION, VirtualQuery};

    let wide = HSTRING::from(name);
    // SAFETY: the name outlives the call; the handle is owned below.
    let Ok(theirs) = (unsafe { OpenFileMappingW(FILE_MAP_READ.0, false, &wide) }) else {
        return Ok(None);
    };
    let theirs = Owned(theirs);

    // A length of zero is "as much as there is", which is the entire point.
    // SAFETY: a valid section handle.
    let at = unsafe { MapViewOfFile(theirs.0, FILE_MAP_READ, 0, 0, 0) };
    if at.Value.is_null() {
        return Err(io::Error::other(format!(
            "{name} exists but could not be viewed"
        )));
    }
    let view = View { at, bytes: 0 };

    let mut about = MEMORY_BASIC_INFORMATION::default();
    // SAFETY: `at` is a mapped address in this process, and `about` is the
    // structure the call is documented to fill.
    let written = unsafe {
        VirtualQuery(
            Some(view.at.Value),
            &mut about,
            core::mem::size_of::<MEMORY_BASIC_INFORMATION>(),
        )
    };
    if written == 0 {
        return Err(io::Error::other(format!(
            "{name} is mapped but Windows would not say how big it is"
        )));
    }

    Ok(Some(crate::probe::Found {
        name: name.to_string(),
        region: about.RegionSize,
    }))
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

/// Where the window of a program by one of these names is, if it has one.
///
/// Every top-level window is looked at; the ones belonging to a process whose
/// image is one of `names` are measured, and [`crate::window::pick`] chooses
/// among them. Owned windows — dialogs, tool windows — are not candidates:
/// a game's window is top level.
pub fn find_window(names: &[String]) -> crate::window::Seen {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GetClientRect, GetForegroundWindow, GetSystemMetrics, GetWindow,
        GetWindowThreadProcessId, IsIconic, IsWindowVisible, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };

    struct Search<'a> {
        names: &'a [String],
        found: Vec<crate::window::Seen>,
        foreground: HWND,
        screen: (i32, i32),
        /// Image names already looked up, by process id: one lookup per
        /// process rather than per window.
        known: Vec<(u32, bool)>,
    }

    unsafe extern "system" fn each(window: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the `Search` handed to `EnumWindows` below, which
        // outlives the enumeration and is not touched anywhere else during it.
        let search = unsafe { &mut *(data.0 as *mut Search<'_>) };
        // SAFETY: plain queries about a window handle the system just handed
        // over; none of them keep it.
        unsafe {
            if !IsWindowVisible(window).as_bool() {
                return BOOL(1);
            }
            if GetWindow(window, GW_OWNER).is_ok_and(|owner| !owner.is_invalid()) {
                return BOOL(1);
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(window, Some(&mut pid));
            let ours = match search.known.iter().find(|(id, _)| *id == pid) {
                Some((_, ours)) => *ours,
                None => {
                    let ours = image_of(pid)
                        .is_some_and(|image| crate::window::is_named(&image, search.names));
                    search.known.push((pid, ours));
                    ours
                }
            };
            if !ours {
                return BOOL(1);
            }
            let minimised = IsIconic(window).as_bool();
            let mut rect = RECT::default();
            let mut corner = POINT::default();
            let measured = GetClientRect(window, &mut rect).is_ok()
                && ClientToScreen(window, &mut corner).as_bool();
            search.found.push(crate::window::Seen {
                found: true,
                minimised,
                foreground: window == search.foreground,
                x: if measured { corner.x } else { 0 },
                y: if measured { corner.y } else { 0 },
                width: if measured && !minimised {
                    rect.right - rect.left
                } else {
                    0
                },
                height: if measured && !minimised {
                    rect.bottom - rect.top
                } else {
                    0
                },
                screen_x: search.screen.0,
                screen_y: search.screen.1,
            });
        }
        BOOL(1)
    }

    // SAFETY: two queries with no arguments that matter.
    let (foreground, screen) = unsafe {
        (
            GetForegroundWindow(),
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
            ),
        )
    };
    let mut search = Search {
        names,
        found: Vec::new(),
        foreground,
        screen,
        known: Vec::new(),
    };
    // SAFETY: the callback only reads through the pointer while `search` is
    // alive, which it is for the whole of this call.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut search as *mut Search<'_> as isize)) };
    crate::window::pick(&search.found)
}

/// The full image path of a process, if it can be asked.
fn image_of(pid: u32) -> Option<String> {
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::core::PWSTR;

    // SAFETY: the handle is closed on every path out, and the buffer is ours
    // and as long as the length we pass says.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        let named = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        );
        let _ = CloseHandle(process);
        named.ok()?;
        Some(String::from_utf16_lossy(&buffer[..length as usize]))
    }
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
