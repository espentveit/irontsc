//! Files copied in the session, fetched to this machine so they can be pasted here.
//!
//! A file list on the clipboard is not the files. What crosses first is a set of descriptions --
//! names, sizes, times -- and the bytes are asked for afterwards, one range at a time, with each
//! answer arriving separately. So a paste on this side has to be arranged in advance: the whole
//! lot is fetched into a directory of its own, and only when it is all here does the list go on
//! the clipboard, pointing at real files a file manager can copy.
//!
//! Which means a large copy is a wait. There is no way around it that keeps the paste honest:
//! the alternative is offering names that only become files if the session is still connected
//! when someone pastes them.

use std::fs::File;
use std::io::Write as _;
use std::path::PathBuf;

use ironrdp::cliprdr::pdu::{
    ClipboardFileAttributes, FileContentsFlags, FileContentsRequest, FileDescriptor,
};
use tracing::{debug, info, warn};

/// How much of a file to ask for at a time.
///
/// Large enough that a big file is not a thousand round trips, small enough that one answer is
/// not a stall: the channel this travels on carries the screen as well.
const CHUNK: u32 = 256 * 1024;

/// A batch of files being fetched from the session.
#[derive(Debug)]
pub struct Incoming {
    /// Where they are being put: a directory of this batch's own, so a second copy of the same
    /// names does not overwrite the first while something is still pasting it.
    directory: PathBuf,
    files: Vec<FileDescriptor>,
    /// Which of them is being fetched.
    index: usize,
    /// How much of it has arrived.
    written: u64,
    handle: Option<File>,
    /// What has been finished, in the order the session listed it.
    done: Vec<PathBuf>,
    /// The number on the request outstanding, so a late answer to an abandoned one is ignored.
    stream: u32,
}

impl Incoming {
    /// Starts a batch, and returns the first thing to ask for.
    ///
    /// `None` means there was nothing to fetch, which is not a failure: a list of empty
    /// directories is complete the moment they are made.
    pub fn start(files: Vec<FileDescriptor>) -> Option<(Self, FileContentsRequest)> {
        let directory = std::env::temp_dir().join(format!(
            "irontsc-clipboard-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_millis())
        ));
        if let Err(error) = std::fs::create_dir_all(&directory) {
            warn!(%error, "📋 nowhere to put the files the session copied");
            return None;
        }

        info!(
            files = files.len(),
            directory = %directory.display(),
            "📋 fetching the files the session copied"
        );

        let mut incoming = Self {
            directory,
            files,
            index: 0,
            written: 0,
            handle: None,
            done: Vec::new(),
            stream: 0,
        };

        incoming.open_current().map(|request| (incoming, request))
    }

    /// Where a file goes, refusing anything that would land outside the batch's directory.
    ///
    /// Names in a list may carry subdirectories, and the separator is the one Windows uses.
    /// Nothing else about them is trusted.
    fn destination(&self, name: &str) -> Option<PathBuf> {
        let mut path = self.directory.clone();
        for part in name.split(['\\', '/']) {
            match part {
                "" | "." => continue,
                ".." => return None,
                part => path.push(part),
            }
        }
        (path != self.directory).then_some(path)
    }

    /// Makes ready whatever is next, and says what to ask for.
    ///
    /// Directories are made rather than fetched, so this walks past them until it finds a file
    /// with something in it or runs out.
    fn open_current(&mut self) -> Option<FileContentsRequest> {
        while let Some(descriptor) = self.files.get(self.index) {
            let Some(path) = self.destination(&descriptor.name) else {
                warn!(name = %descriptor.name, "📋 skipping a file with a name that leads out");
                self.index += 1;
                continue;
            };

            let is_directory = descriptor
                .attributes
                .is_some_and(|attributes| attributes.contains(ClipboardFileAttributes::DIRECTORY));

            if is_directory {
                if let Err(error) = std::fs::create_dir_all(&path) {
                    warn!(%error, path = %path.display(), "📋 could not make a folder");
                }
                self.done.push(path);
                self.index += 1;
                continue;
            }

            if let Some(parent) = path.parent()
                && let Err(error) = std::fs::create_dir_all(parent)
            {
                warn!(%error, "📋 could not make a folder to put a file in");
                self.index += 1;
                continue;
            }

            match File::create(&path) {
                Ok(handle) => {
                    self.handle = Some(handle);
                    self.written = 0;
                    self.done.push(path);

                    // A file with nothing in it is finished as soon as it exists.
                    if descriptor.file_size == Some(0) {
                        self.handle = None;
                        self.index += 1;
                        continue;
                    }

                    return Some(self.ask());
                }
                Err(error) => {
                    warn!(%error, path = %path.display(), "📋 could not make a file");
                    self.index += 1;
                }
            }
        }

        None
    }

    /// The request for the next piece of the file being fetched.
    fn ask(&mut self) -> FileContentsRequest {
        self.stream = self.stream.wrapping_add(1);
        FileContentsRequest {
            stream_id: self.stream,
            index: self.index as u32,
            flags: FileContentsFlags::DATA,
            position: self.written,
            requested_size: CHUNK,
            data_id: None,
        }
    }

    /// Takes one answer. Returns the next request, or nothing when the batch is done.
    pub fn receive(&mut self, stream_id: u32, data: &[u8]) -> Next {
        if stream_id != self.stream {
            debug!(stream_id, "📋 an answer to a request that was abandoned");
            return Next::Waiting;
        }

        let expected = self.files.get(self.index).and_then(|file| file.file_size);
        let mut finished = data.is_empty();

        if let Some(handle) = self.handle.as_mut() {
            if let Err(error) = handle.write_all(data) {
                warn!(%error, "📋 could not write a file the session copied");
                finished = true;
            } else {
                self.written += data.len() as u64;
            }
        }

        // Done with this one when it has reached the size that was promised, or when the
        // session answered with less than was asked for, which is how a file of unknown size
        // ends.
        if expected.is_some_and(|size| self.written >= size) || (data.len() as u32) < CHUNK {
            finished = true;
        }

        if !finished {
            return Next::Ask(self.ask());
        }

        self.handle = None;
        self.index += 1;
        match self.open_current() {
            Some(request) => Next::Ask(request),
            None => Next::Done(std::mem::take(&mut self.done)),
        }
    }

    /// Gives up on the batch, leaving whatever arrived where it is.
    pub fn abandon(&mut self) {
        self.handle = None;
        self.stream = self.stream.wrapping_add(1);
    }
}

/// What to do after an answer.
#[derive(Debug)]
pub enum Next {
    /// Ask for this.
    Ask(FileContentsRequest),
    /// Everything is here, at these paths.
    Done(Vec<PathBuf>),
    /// Nothing to do; the answer was not one this was waiting for.
    Waiting,
}
