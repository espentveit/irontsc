//! Folders from this machine, in the session's File Explorer (MS-RDPEFS).
//!
//! A share is a local directory given a name. The server sees a drive, and every open, read,
//! write and directory listing arrives here as an I/O request against a path inside it.
//!
//! Two ways in, and the difference is only when. A connection may carry shares from the start,
//! the way mstsc's `drivestoredirect` does, and they are announced during the initial handshake.
//! Or one may be added while the session is running -- the protocol allows a device list to be
//! announced at any point, which is how a memory stick plugged in mid-session appears -- which
//! is what an agent driving the session needs, since what it wants to share is rarely known
//! before it starts.
//!
//! Nothing outside a share's directory can be reached: every path from the server is resolved
//! against the share root and refused if it climbs out.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ironrdp::rdpdr::pdu::efs::{
    ClientDriveQueryDirectoryResponse, ClientDriveQueryInformationResponse,
    ClientDriveQueryVolumeInformationResponse, ClientDriveSetInformationResponse,
    CreateDisposition, CreateOptions, DeviceCloseResponse, DeviceControlResponse,
    DeviceCreateRequest, DeviceCreateResponse, DeviceIoRequest, DeviceIoResponse,
    DeviceReadRequest, DeviceReadResponse, DeviceWriteRequest, DeviceWriteResponse,
    FileAttributes, FileBasicInformation, FileBothDirectoryInformation, FileDirectoryInformation,
    FileFsAttributeInformation, FileFsDeviceInformation, FileFsFullSizeInformation,
    FileFsSizeInformation, FileFsVolumeInformation, FileFullDirectoryInformation,
    FileInformationClass, FileInformationClassLevel, FileNamesInformation, FileStandardInformation,
    Boolean, Characteristics, FileAttributeTagInformation, FileSystemAttributes,
    FileSystemInformationClass, FileSystemInformationClassLevel, Information,
    NtStatus, ServerDriveIoRequest, ServerDriveQueryDirectoryRequest,
    ServerDriveQueryInformationRequest, ServerDriveQueryVolumeInformationRequest,
    ServerDriveSetInformationRequest,
};
use ironrdp::rdpdr::pdu::RdpdrPdu;
use ironrdp::svc::SvcMessage;
use tracing::{debug, info, warn};

/// The epoch Windows counts file times from, in seconds before the Unix one.
const FILETIME_EPOCH_OFFSET: i64 = 11_644_473_600;
/// Windows file times are in hundreds of nanoseconds.
const FILETIME_TICKS_PER_SECOND: i64 = 10_000_000;

/// A local directory offered to the session under a name.
#[derive(Debug, Clone)]
pub struct Share {
    /// What the session calls it, which is what File Explorer shows.
    pub name: String,
    /// Where it actually is.
    pub root: PathBuf,
    /// Whether the session may change anything in it.
    pub writable: bool,
}

/// Reads a share list: `name=/path` entries separated by semicolons, `:ro` for read-only.
///
/// A path with no name takes the last part of the path as its name, so `/home/espen/notes`
/// alone appears in the session as "notes".
pub fn parse_shares(list: &str) -> Vec<Share> {
    let mut shares = Vec::new();

    for entry in list.split(';') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }

        let (entry, writable) = match entry.strip_suffix(":ro") {
            Some(entry) => (entry, false),
            None => (entry, true),
        };
        let (name, path) = match entry.split_once('=') {
            Some((name, path)) => (name.trim().to_owned(), PathBuf::from(path.trim())),
            None => {
                let path = PathBuf::from(entry);
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "share".to_owned());
                (name, path)
            }
        };

        if !path.is_dir() {
            warn!(path = %path.display(), "🗂 not a folder, not sharing it");
            continue;
        }

        shares.push(Share {
            name,
            root: path,
            writable,
        });
    }

    shares
}

/// One file or directory the session has open.
#[derive(Debug)]
struct OpenFile {
    device: u32,
    path: PathBuf,
    is_directory: bool,
    /// Set by `FILE_DELETE_ON_CLOSE`, or by a disposition that asks for it later.
    delete_on_close: bool,
    /// A directory listing in progress, as name and path. The server asks for one entry at a
    /// time, and the name is carried separately because `.` and `..` have no file name of
    /// their own to take one from.
    listing: Option<std::vec::IntoIter<(String, PathBuf)>>,
}

/// The shares this client offers, and the files the session has open in them.
#[derive(Debug, Default)]
pub struct Drives {
    shares: BTreeMap<u32, Share>,
    files: BTreeMap<u32, OpenFile>,
    next_file_id: u32,
}

impl Drives {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes a share into the set, so its I/O can be served.
    ///
    /// Announcing it to the server is separate, and the caller's: a share added before the
    /// connection goes out with the initial device list, and one added afterwards goes out on
    /// its own.
    pub fn insert(&mut self, device_id: u32, share: Share) {
        info!(
            device_id,
            name = %share.name,
            root = %share.root.display(),
            writable = share.writable,
            "🗂 sharing a folder with the session"
        );
        self.shares.insert(device_id, share);
    }

    /// Drops a share, and any files the session still had open in it.
    pub fn remove(&mut self, device_id: u32) -> Option<Share> {
        self.files.retain(|_, file| file.device != device_id);
        self.shares.remove(&device_id)
    }

    /// What is shared, by device id.
    pub fn shares(&self) -> impl Iterator<Item = (u32, &Share)> {
        self.shares.iter().map(|(id, share)| (*id, share))
    }

    /// The id a new share should take, which is one past the highest in use.
    pub fn next_device_id(&self) -> u32 {
        self.shares.keys().copied().max().map_or(2, |id| id + 1)
    }

    /// Resolves a path the server sent against a share, refusing anything that climbs out.
    ///
    /// The server speaks in Windows paths -- backslashes, and a leading one for the root of the
    /// drive. A component of `..` is not a path this client is willing to follow, and neither
    /// is a symbolic link that leads out of the share, which is why the result is compared
    /// against the canonical root rather than merely assembled.
    fn resolve(&self, device: u32, path: &str) -> Option<PathBuf> {
        let share = self.shares.get(&device)?;
        let mut resolved = share.root.clone();

        for part in path.split(['\\', '/']) {
            match part {
                "" | "." => continue,
                ".." => return None,
                part => resolved.push(part),
            }
        }

        // A path that does not exist yet cannot be canonicalised, so its parent is checked
        // instead: that is where a create would put it.
        let (to_check, tail) = if resolved.exists() {
            (resolved.clone(), None)
        } else {
            let parent = resolved.parent()?.to_path_buf();
            (parent, resolved.file_name().map(|name| name.to_owned()))
        };

        let root = share.root.canonicalize().ok()?;
        let inside = to_check.canonicalize().ok()?;
        if !inside.starts_with(&root) {
            warn!(path, "🗂 refusing a path that leads out of the share");
            return None;
        }

        Some(match tail {
            Some(name) => inside.join(name),
            None => inside,
        })
    }

    fn writable(&self, device: u32) -> bool {
        self.shares.get(&device).is_some_and(|share| share.writable)
    }

    /// Serves one I/O request from the session.
    pub fn handle(&mut self, request: ServerDriveIoRequest) -> Vec<SvcMessage> {
        let (device, kind) = match &request {
            ServerDriveIoRequest::ServerCreateDriveRequest(r) => {
                (r.device_io_request.device_id, "create")
            }
            ServerDriveIoRequest::DeviceCloseRequest(r) => (r.device_io_request.device_id, "close"),
            ServerDriveIoRequest::ServerDriveQueryInformationRequest(r) => {
                (r.device_io_request.device_id, "query information")
            }
            ServerDriveIoRequest::ServerDriveQueryDirectoryRequest(r) => {
                (r.device_io_request.device_id, "query directory")
            }
            ServerDriveIoRequest::ServerDriveQueryVolumeInformationRequest(r) => {
                (r.device_io_request.device_id, "query volume")
            }
            ServerDriveIoRequest::DeviceReadRequest(r) => (r.device_io_request.device_id, "read"),
            ServerDriveIoRequest::DeviceWriteRequest(r) => (r.device_io_request.device_id, "write"),
            ServerDriveIoRequest::ServerDriveSetInformationRequest(r) => {
                (r.device_io_request.device_id, "set information")
            }
            ServerDriveIoRequest::ServerDriveNotifyChangeDirectoryRequest(r) => {
                (r.device_io_request.device_id, "watch directory")
            }
            ServerDriveIoRequest::ServerDriveLockControlRequest(r) => {
                (r.device_io_request.device_id, "lock")
            }
            ServerDriveIoRequest::DeviceControlRequest(r) => (r.header.device_id, "control"),
        };
        debug!(device, kind, "🗂 request from the session");

        let pdu = match request {
            ServerDriveIoRequest::ServerCreateDriveRequest(request) => self.create(request),
            ServerDriveIoRequest::DeviceCloseRequest(request) => {
                let io = request.device_io_request;
                self.close(io.file_id);
                RdpdrPdu::DeviceCloseResponse(DeviceCloseResponse {
                    device_io_response: DeviceIoResponse::new(io, NtStatus::SUCCESS),
                })
            }
            ServerDriveIoRequest::ServerDriveQueryInformationRequest(request) => {
                self.query_information(request)
            }
            ServerDriveIoRequest::ServerDriveQueryDirectoryRequest(request) => {
                self.query_directory(request)
            }
            ServerDriveIoRequest::ServerDriveQueryVolumeInformationRequest(request) => {
                self.query_volume(request)
            }
            ServerDriveIoRequest::DeviceReadRequest(request) => self.read(request),
            ServerDriveIoRequest::DeviceWriteRequest(request) => self.write(request),
            ServerDriveIoRequest::ServerDriveSetInformationRequest(request) => {
                self.set_information(request)
            }
            // A directory watch this client never satisfies: answering at all would be a
            // promise to report changes, and never answering is what the specification allows
            // for a client that does not.
            ServerDriveIoRequest::ServerDriveNotifyChangeDirectoryRequest(_) => return Vec::new(),
            ServerDriveIoRequest::ServerDriveLockControlRequest(request) => {
                RdpdrPdu::EmptyResponse.pipe(DeviceIoResponse::new(
                    request.device_io_request,
                    NtStatus::SUCCESS,
                ))
            }
            ServerDriveIoRequest::DeviceControlRequest(request) => {
                RdpdrPdu::DeviceControlResponse(DeviceControlResponse::new(
                    request,
                    NtStatus::NOT_SUPPORTED,
                    None,
                ))
            }
        };

        vec![SvcMessage::from(pdu)]
    }

    fn create(&mut self, request: DeviceCreateRequest) -> RdpdrPdu {
        let io = request.device_io_request.clone();
        let device = io.device_id;

        let Some(path) = self.resolve(device, &request.path) else {
            return create_failed(io, NtStatus::ACCESS_DENIED);
        };

        let wants_directory = request
            .create_options
            .contains(CreateOptions::FILE_DIRECTORY_FILE);
        let exists = path.exists();
        let disposition = request.create_disposition;
        let writable = self.writable(device);

        let changes = matches!(
            disposition,
            CreateDisposition::FILE_CREATE
                | CreateDisposition::FILE_SUPERSEDE
                | CreateDisposition::FILE_OVERWRITE
                | CreateDisposition::FILE_OVERWRITE_IF
        ) || (disposition == CreateDisposition::FILE_OPEN_IF && !exists);
        if changes && !writable {
            return create_failed(io, NtStatus::ACCESS_DENIED);
        }

        let information = match (disposition, exists) {
            (CreateDisposition::FILE_OPEN, false) => {
                return create_failed(io, NtStatus::NO_SUCH_FILE)
            }
            (CreateDisposition::FILE_CREATE, true) => {
                return create_failed(io, NtStatus::OBJECT_NAME_COLLISION)
            }
            (CreateDisposition::FILE_OPEN | CreateDisposition::FILE_OPEN_IF, true) => {
                Information::FILE_OPENED
            }
            (CreateDisposition::FILE_OVERWRITE, false) => {
                return create_failed(io, NtStatus::NO_SUCH_FILE)
            }
            (CreateDisposition::FILE_OVERWRITE | CreateDisposition::FILE_OVERWRITE_IF, true) => {
                if File::create(&path).is_err() {
                    return create_failed(io, NtStatus::UNSUCCESSFUL);
                }
                Information::FILE_OVERWRITTEN
            }
            _ => {
                // Everything left over is a create.
                let made = if wants_directory {
                    std::fs::create_dir(&path).is_ok()
                } else {
                    File::create(&path).is_ok()
                };
                if !made {
                    return create_failed(io, NtStatus::UNSUCCESSFUL);
                }
                Information::FILE_SUPERSEDED
            }
        };

        let is_directory = path.is_dir();
        if wants_directory && !is_directory {
            return create_failed(io, NtStatus::NOT_A_DIRECTORY);
        }

        self.next_file_id = self.next_file_id.wrapping_add(1);
        let file_id = self.next_file_id;
        self.files.insert(
            file_id,
            OpenFile {
                device,
                path,
                is_directory,
                delete_on_close: request
                    .create_options
                    .contains(CreateOptions::FILE_DELETE_ON_CLOSE),
                listing: None,
            },
        );

        RdpdrPdu::DeviceCreateResponse(DeviceCreateResponse {
            device_io_reply: DeviceIoResponse::new(io, NtStatus::SUCCESS),
            file_id,
            information,
        })
    }

    fn close(&mut self, file_id: u32) {
        let Some(file) = self.files.remove(&file_id) else {
            return;
        };
        if file.delete_on_close {
            let removed = if file.is_directory {
                std::fs::remove_dir(&file.path)
            } else {
                std::fs::remove_file(&file.path)
            };
            if let Err(error) = removed {
                debug!(%error, path = %file.path.display(), "🗂 could not delete on close");
            }
        }
    }

    fn query_information(&mut self, request: ServerDriveQueryInformationRequest) -> RdpdrPdu {
        let io = request.device_io_request.clone();
        let Some(file) = self.files.get(&io.file_id) else {
            return RdpdrPdu::ClientDriveQueryInformationResponse(
                ClientDriveQueryInformationResponse {
                    device_io_response: DeviceIoResponse::new(io, NtStatus::UNSUCCESSFUL),
                    buffer: None,
                },
            );
        };
        let Ok(metadata) = file.path.metadata() else {
            return RdpdrPdu::ClientDriveQueryInformationResponse(
                ClientDriveQueryInformationResponse {
                    device_io_response: DeviceIoResponse::new(io, NtStatus::NO_SUCH_FILE),
                    buffer: None,
                },
            );
        };

        let times = FileTimes::of(&metadata);
        let attributes = attributes_of(&metadata);
        let buffer = match request.file_info_class_lvl {
            FileInformationClassLevel::FILE_BASIC_INFORMATION => {
                Some(FileInformationClass::Basic(FileBasicInformation {
                    creation_time: times.creation,
                    last_access_time: times.access,
                    last_write_time: times.write,
                    change_time: times.write,
                    file_attributes: attributes,
                }))
            }
            FileInformationClassLevel::FILE_STANDARD_INFORMATION => {
                Some(FileInformationClass::Standard(FileStandardInformation {
                    allocation_size: metadata.len() as i64,
                    end_of_file: metadata.len() as i64,
                    number_of_links: 0,
                    delete_pending: yes_no(file.delete_on_close),
                    directory: yes_no(metadata.is_dir()),
                }))
            }
            FileInformationClassLevel::FILE_ATTRIBUTE_TAG_INFORMATION => Some(
                FileInformationClass::AttributeTag(FileAttributeTagInformation {
                    file_attributes: attributes,
                    reparse_tag: 0,
                }),
            ),
            other => {
                debug!(?other, "🗂 unsupported file information class");
                return RdpdrPdu::ClientDriveQueryInformationResponse(
                    ClientDriveQueryInformationResponse {
                        device_io_response: DeviceIoResponse::new(io, NtStatus::NOT_SUPPORTED),
                        buffer: None,
                    },
                );
            }
        };

        RdpdrPdu::ClientDriveQueryInformationResponse(ClientDriveQueryInformationResponse {
            device_io_response: DeviceIoResponse::new(io, NtStatus::SUCCESS),
            buffer,
        })
    }

    fn query_directory(&mut self, request: ServerDriveQueryDirectoryRequest) -> RdpdrPdu {
        let io = request.device_io_request.clone();
        let no_more = |io: DeviceIoRequest| {
            RdpdrPdu::ClientDriveQueryDirectoryResponse(ClientDriveQueryDirectoryResponse {
                device_io_reply: DeviceIoResponse::new(io, NtStatus::NO_MORE_FILES),
                buffer: None,
            })
        };

        let Some(file) = self.files.get_mut(&io.file_id) else {
            return no_more(io);
        };

        // The first request opens the listing; the rest walk it. "." and ".." come first, the
        // way a real drive presents them, or File Explorer has nowhere to go up to.
        if request.initial_query != 0 {
            let parent = file.path.parent().unwrap_or(&file.path).to_path_buf();
            let mut entries = vec![
                (".".to_owned(), file.path.clone()),
                ("..".to_owned(), parent),
            ];
            match std::fs::read_dir(&file.path) {
                Ok(reader) => entries.extend(reader.flatten().map(|entry| {
                    (entry.file_name().to_string_lossy().into_owned(), entry.path())
                })),
                Err(error) => {
                    debug!(%error, path = %file.path.display(), "🗂 could not list a directory");
                    return no_more(io);
                }
            }
            file.listing = Some(entries.into_iter());
        }

        let Some((name, entry)) = file.listing.as_mut().and_then(Iterator::next) else {
            return no_more(io);
        };

        let metadata = entry.metadata().ok();
        let times = metadata.as_ref().map(FileTimes::of).unwrap_or_default();
        let size = metadata.as_ref().map_or(0, |m| m.len()) as i64;
        let attributes = metadata.as_ref().map_or(
            FileAttributes::FILE_ATTRIBUTE_DIRECTORY,
            |m| attributes_of(m),
        );

        let buffer = match request.file_info_class_lvl {
            FileInformationClassLevel::FILE_BOTH_DIRECTORY_INFORMATION => {
                FileInformationClass::BothDirectory(FileBothDirectoryInformation::new(
                    times.creation,
                    times.access,
                    times.write,
                    times.write,
                    size,
                    attributes,
                    name,
                ))
            }
            FileInformationClassLevel::FILE_FULL_DIRECTORY_INFORMATION => {
                FileInformationClass::FullDirectory(FileFullDirectoryInformation::new(
                    times.creation,
                    times.access,
                    times.write,
                    times.write,
                    size,
                    attributes,
                    name,
                ))
            }
            FileInformationClassLevel::FILE_DIRECTORY_INFORMATION => {
                FileInformationClass::Directory(FileDirectoryInformation::new(
                    times.creation,
                    times.access,
                    times.write,
                    times.write,
                    size,
                    attributes,
                    name,
                ))
            }
            FileInformationClassLevel::FILE_NAMES_INFORMATION => {
                FileInformationClass::Names(FileNamesInformation::new(name))
            }
            other => {
                debug!(?other, "🗂 unsupported directory information class");
                return no_more(io);
            }
        };

        RdpdrPdu::ClientDriveQueryDirectoryResponse(ClientDriveQueryDirectoryResponse {
            device_io_reply: DeviceIoResponse::new(io, NtStatus::SUCCESS),
            buffer: Some(buffer),
        })
    }

    fn query_volume(&mut self, request: ServerDriveQueryVolumeInformationRequest) -> RdpdrPdu {
        let io = request.device_io_request.clone();
        let label = self
            .shares
            .get(&io.device_id)
            .map(|share| share.name.clone())
            .unwrap_or_default();

        // Sizes this client does not measure. A share is a directory, not a volume, and the
        // free space of the filesystem behind it is not what the session is being offered.
        const CLUSTERS: i64 = 1_048_576;
        const SECTORS_PER_CLUSTER: u32 = 8;
        const BYTES_PER_SECTOR: u32 = 512;

        let buffer = match request.fs_info_class_lvl {
            FileSystemInformationClassLevel::FILE_FS_VOLUME_INFORMATION => {
                Some(FileSystemInformationClass::FileFsVolumeInformation(
                    FileFsVolumeInformation {
                        volume_creation_time: 0,
                        volume_serial_number: 0,
                        supports_objects: Boolean::False,
                        volume_label: label,
                    },
                ))
            }
            FileSystemInformationClassLevel::FILE_FS_SIZE_INFORMATION => {
                Some(FileSystemInformationClass::FileFsSizeInformation(
                    FileFsSizeInformation {
                        total_alloc_units: CLUSTERS,
                        available_alloc_units: CLUSTERS / 2,
                        sectors_per_alloc_unit: SECTORS_PER_CLUSTER,
                        bytes_per_sector: BYTES_PER_SECTOR,
                    },
                ))
            }
            FileSystemInformationClassLevel::FILE_FS_FULL_SIZE_INFORMATION => {
                Some(FileSystemInformationClass::FileFsFullSizeInformation(
                    FileFsFullSizeInformation {
                        total_alloc_units: CLUSTERS,
                        caller_available_alloc_units: CLUSTERS / 2,
                        actual_available_alloc_units: CLUSTERS / 2,
                        sectors_per_alloc_unit: SECTORS_PER_CLUSTER,
                        bytes_per_sector: BYTES_PER_SECTOR,
                    },
                ))
            }
            FileSystemInformationClassLevel::FILE_FS_ATTRIBUTE_INFORMATION => {
                Some(FileSystemInformationClass::FileFsAttributeInformation(
                    FileFsAttributeInformation {
                        file_system_attributes: FileSystemAttributes::FILE_CASE_SENSITIVE_SEARCH
                            | FileSystemAttributes::FILE_CASE_PRESERVED_NAMES
                            | FileSystemAttributes::FILE_UNICODE_ON_DISK,
                        max_component_name_len: 255,
                        file_system_name: "IRONTSC".to_owned(),
                    },
                ))
            }
            FileSystemInformationClassLevel::FILE_FS_DEVICE_INFORMATION => {
                Some(FileSystemInformationClass::FileFsDeviceInformation(
                    FileFsDeviceInformation {
                        // A disk, and one that may go away, which is what a share is.
                        device_type: 0x07,
                        characteristics: Characteristics::FILE_REMOTE_DEVICE,
                    },
                ))
            }
            other => {
                debug!(?other, "🗂 unsupported volume information class");
                return RdpdrPdu::ClientDriveQueryVolumeInformationResponse(
                    ClientDriveQueryVolumeInformationResponse::new(
                        io,
                        NtStatus::NOT_SUPPORTED,
                        None,
                    ),
                );
            }
        };

        RdpdrPdu::ClientDriveQueryVolumeInformationResponse(
            ClientDriveQueryVolumeInformationResponse::new(io, NtStatus::SUCCESS, buffer),
        )
    }

    fn read(&mut self, request: DeviceReadRequest) -> RdpdrPdu {
        let io = request.device_io_request.clone();
        let read = self.files.get(&io.file_id).and_then(|file| {
            let mut handle = File::open(&file.path).ok()?;
            handle.seek(SeekFrom::Start(request.offset)).ok()?;
            let mut data = vec![0u8; request.length as usize];
            let read = handle.read(&mut data).ok()?;
            data.truncate(read);
            Some(data)
        });

        match read {
            Some(read_data) => RdpdrPdu::DeviceReadResponse(DeviceReadResponse {
                device_io_reply: DeviceIoResponse::new(io, NtStatus::SUCCESS),
                read_data,
            }),
            None => RdpdrPdu::DeviceReadResponse(DeviceReadResponse {
                device_io_reply: DeviceIoResponse::new(io, NtStatus::UNSUCCESSFUL),
                read_data: Vec::new(),
            }),
        }
    }

    fn write(&mut self, request: DeviceWriteRequest) -> RdpdrPdu {
        let io = request.device_io_request.clone();
        if !self.writable(io.device_id) {
            return RdpdrPdu::DeviceWriteResponse(DeviceWriteResponse {
                device_io_reply: DeviceIoResponse::new(io, NtStatus::ACCESS_DENIED),
                length: 0,
            });
        }

        let written = self.files.get(&io.file_id).and_then(|file| {
            let mut handle = std::fs::OpenOptions::new().write(true).open(&file.path).ok()?;
            handle.seek(SeekFrom::Start(request.offset)).ok()?;
            handle.write_all(&request.write_data).ok()?;
            Some(request.write_data.len() as u32)
        });

        match written {
            Some(length) => RdpdrPdu::DeviceWriteResponse(DeviceWriteResponse {
                device_io_reply: DeviceIoResponse::new(io, NtStatus::SUCCESS),
                length,
            }),
            None => RdpdrPdu::DeviceWriteResponse(DeviceWriteResponse {
                device_io_reply: DeviceIoResponse::new(io, NtStatus::UNSUCCESSFUL),
                length: 0,
            }),
        }
    }

    fn set_information(&mut self, request: ServerDriveSetInformationRequest) -> RdpdrPdu {
        let io = request.device_io_request.clone();
        let status = if !self.writable(io.device_id) {
            NtStatus::ACCESS_DENIED
        } else {
            self.apply_set_information(&request, io.file_id)
        };

        match ClientDriveSetInformationResponse::new(&request, status) {
            Ok(response) => RdpdrPdu::ClientDriveSetInformationResponse(response),
            Err(error) => {
                warn!(%error, "🗂 could not answer a set information request");
                RdpdrPdu::EmptyResponse.pipe(DeviceIoResponse::new(io, NtStatus::UNSUCCESSFUL))
            }
        }
    }

    fn apply_set_information(
        &mut self,
        request: &ServerDriveSetInformationRequest,
        file_id: u32,
    ) -> NtStatus {
        let Some((device, path)) = self
            .files
            .get(&file_id)
            .map(|file| (file.device, file.path.clone()))
        else {
            return NtStatus::UNSUCCESSFUL;
        };

        match &request.set_buffer {
            // Truncating or extending, which is how a file gets its final size after a write.
            FileInformationClass::EndOfFile(end) => {
                match std::fs::OpenOptions::new().write(true).open(&path) {
                    Ok(handle) => match handle.set_len(end.end_of_file as u64) {
                        Ok(()) => NtStatus::SUCCESS,
                        Err(_) => NtStatus::UNSUCCESSFUL,
                    },
                    Err(_) => NtStatus::UNSUCCESSFUL,
                }
            }
            // The space a file is allowed; this filesystem gives what it needs.
            FileInformationClass::Allocation(_) => NtStatus::SUCCESS,
            FileInformationClass::Disposition(disposition) => {
                if let Some(file) = self.files.get_mut(&file_id) {
                    file.delete_on_close = disposition.delete_pending != 0;
                }
                NtStatus::SUCCESS
            }
            FileInformationClass::Rename(rename) => {
                let Some(target) = self.resolve(device, &rename.file_name) else {
                    return NtStatus::ACCESS_DENIED;
                };
                if target.exists() && matches!(rename.replace_if_exists, Boolean::False) {
                    return NtStatus::OBJECT_NAME_COLLISION;
                }
                let Some(file) = self.files.get_mut(&file_id) else {
                    return NtStatus::UNSUCCESSFUL;
                };
                match std::fs::rename(&path, &target) {
                    Ok(()) => {
                        file.path = target;
                        NtStatus::SUCCESS
                    }
                    Err(_) => NtStatus::UNSUCCESSFUL,
                }
            }
            // Timestamps and attributes, which this client does not carry over.
            FileInformationClass::Basic(_) => NtStatus::SUCCESS,
            other => {
                debug!(?other, "🗂 unsupported set information class");
                NtStatus::NOT_SUPPORTED
            }
        }
    }
}

/// A file's three times, as Windows counts them.
#[derive(Debug, Default, Clone, Copy)]
struct FileTimes {
    creation: i64,
    access: i64,
    write: i64,
}

impl FileTimes {
    fn of(metadata: &std::fs::Metadata) -> Self {
        Self {
            creation: filetime(metadata.created().ok()),
            access: filetime(metadata.accessed().ok()),
            write: filetime(metadata.modified().ok()),
        }
    }
}

/// Turns a system time into the hundreds of nanoseconds since 1601 that Windows counts in.
fn filetime(time: Option<SystemTime>) -> i64 {
    let Some(time) = time else {
        return 0;
    };
    let Ok(since_epoch) = time.duration_since(UNIX_EPOCH) else {
        return 0;
    };
    (since_epoch.as_secs() as i64 + FILETIME_EPOCH_OFFSET) * FILETIME_TICKS_PER_SECOND
        + i64::from(since_epoch.subsec_nanos() / 100)
}

/// The protocol's own spelling of a boolean.
fn yes_no(value: bool) -> Boolean {
    if value {
        Boolean::True
    } else {
        Boolean::False
    }
}

fn attributes_of(metadata: &std::fs::Metadata) -> FileAttributes {
    let mut attributes = if metadata.is_dir() {
        FileAttributes::FILE_ATTRIBUTE_DIRECTORY
    } else {
        FileAttributes::FILE_ATTRIBUTE_NORMAL
    };
    if metadata.permissions().readonly() {
        attributes |= FileAttributes::FILE_ATTRIBUTE_READONLY;
    }
    attributes
}

fn create_failed(io: DeviceIoRequest, status: NtStatus) -> RdpdrPdu {
    RdpdrPdu::DeviceCreateResponse(DeviceCreateResponse {
        device_io_reply: DeviceIoResponse::new(io, status),
        file_id: 0,
        information: Information::empty(),
    })
}

trait Pipe {
    fn pipe(self, response: DeviceIoResponse) -> RdpdrPdu;
}

impl Pipe for RdpdrPdu {
    /// An acknowledgement with nothing in it beyond the status.
    fn pipe(self, response: DeviceIoResponse) -> RdpdrPdu {
        let _ = self;
        RdpdrPdu::ClientDriveQueryInformationResponse(ClientDriveQueryInformationResponse {
            device_io_response: response,
            buffer: None,
        })
    }
}


/// The shares, as the `rdpdr` channel's backend.
///
/// The set is shared: the channel serves I/O from it on the session thread, while a share added
/// or dropped while the session is running is written from wherever that request came in.
#[derive(Debug, Clone, Default)]
pub struct SharedDrives(std::sync::Arc<std::sync::Mutex<Drives>>);

impl SharedDrives {
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs something against the shares, unless the lock is poisoned.
    pub fn with<T>(&self, act: impl FnOnce(&mut Drives) -> T) -> Option<T> {
        self.0.lock().ok().map(|mut drives| act(&mut drives))
    }
}

impl ironrdp_core::AsAny for SharedDrives {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl ironrdp::rdpdr::RdpdrBackend for SharedDrives {
    fn handle_server_device_announce_response(
        &mut self,
        pdu: ironrdp::rdpdr::pdu::efs::ServerDeviceAnnounceResponse,
    ) -> ironrdp_pdu::PduResult<()> {
        if pdu.result_code == NtStatus::SUCCESS {
            info!(device_id = pdu.device_id, "🗂 the session took the share");
        } else {
            warn!(
                device_id = pdu.device_id,
                status = ?pdu.result_code,
                "🗂 the session refused the share"
            );
        }
        Ok(())
    }

    fn handle_scard_call(
        &mut self,
        _req: ironrdp::rdpdr::pdu::efs::DeviceControlRequest<
            ironrdp::rdpdr::pdu::esc::ScardIoCtlCode,
        >,
        _call: ironrdp::rdpdr::pdu::esc::ScardCall,
    ) -> ironrdp_pdu::PduResult<()> {
        Ok(())
    }

    fn handle_drive_io_request(
        &mut self,
        req: ServerDriveIoRequest,
    ) -> ironrdp_pdu::PduResult<Vec<SvcMessage>> {
        Ok(self.with(|drives| drives.handle(req)).unwrap_or_default())
    }
}
