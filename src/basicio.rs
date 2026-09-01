//! BASICIO system classes (Simula Standard Chapter 10).
//!
//! Injects `File` / `ImageFile` / `InFile` / `OutFile` / `PrintFile` and
//! intercepts their methods like SIMSET. Terminal SysIn/SysOut free wrappers
//! live in [`crate::runtime::io`].

use std::collections::HashMap;
use std::fs::{File as OsFile, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use crate::ast::{Block, ClassDeclaration, FormalParameter, ParamMode, Specification, Specifier};
use crate::runtime::text::TextFrame;
use crate::types::Type;

/// Synthetic object identities reserved for terminal files (interpreter).
pub const SYSIN_IDENTITY: u64 = u64::MAX - 1;
pub const SYSOUT_IDENTITY: u64 = u64::MAX - 2;

/// ISO rank 25 — end-of-medium / EOF image character (§10.4.2).
pub const EM_CHAR: char = '\u{0019}';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileMode {
    In,
    Out,
}

/// Concrete BASICIO subclass kind (§10.2 / §§10.8–10.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    InFile,
    OutFile,
    PrintFile,
    DirectFile,
    InByteFile,
    OutByteFile,
    DirectByteFile,
}

impl FileKind {
    pub fn from_class_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "infile" => Self::InFile,
            "printfile" => Self::PrintFile,
            "directfile" => Self::DirectFile,
            "inbytefile" => Self::InByteFile,
            "outbytefile" => Self::OutByteFile,
            "directbytefile" => Self::DirectByteFile,
            _ => Self::OutFile,
        }
    }

    pub fn is_byte(self) -> bool {
        matches!(
            self,
            Self::InByteFile | Self::OutByteFile | Self::DirectByteFile
        )
    }

    pub fn is_direct(self) -> bool {
        matches!(self, Self::DirectFile | Self::DirectByteFile)
    }
}

/// Implementation-defined SYSIN / SYSOUT image lengths (§10 intro).
pub const DEFAULT_INPUT_LINELENGTH: i64 = 80;
pub const DEFAULT_OUTPUT_LINELENGTH: i64 = 132;
pub const DEFAULT_LINES_PER_PAGE: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateMode {
    Create,
    NoCreate,
    AnyCreate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadWriteMode {
    ReadOnly,
    WriteOnly,
    ReadWrite,
}

/// Pending / effective access modes (§10.1.1).
#[derive(Debug, Clone)]
pub struct AccessModes {
    pub shared: bool,
    pub append: bool,
    pub create: CreateMode,
    pub readwrite: ReadWriteMode,
    pub bytesize: u16,
    pub rewind: bool,
    pub purge: bool,
}

impl AccessModes {
    pub fn defaults_for(mode: FileMode) -> Self {
        match mode {
            FileMode::In => Self {
                shared: true,
                append: false,
                create: CreateMode::AnyCreate, // NA for infile; ignored
                readwrite: ReadWriteMode::ReadOnly,
                bytesize: 8,
                rewind: false,
                purge: false,
            },
            FileMode::Out => Self {
                shared: false,
                append: false,
                create: CreateMode::AnyCreate,
                readwrite: ReadWriteMode::WriteOnly,
                bytesize: 8,
                rewind: false,
                purge: false,
            },
        }
    }
}

#[derive(Debug)]
pub struct FileHandle {
    pub filename: String,
    pub mode: FileMode,
    pub kind: FileKind,
    pub open: bool,
    /// Image buffer established by `open(fileimage)` (§10.3).
    pub image: TextFrame,
    pub endfile: bool,
    pub access: AccessModes,
    /// PrintFile pagination (§10.7).
    pub line: i64,
    pub page: i64,
    pub spacing: i64,
    pub lines_per_page: i64,
    pub default_lines_per_page: i64,
    /// DirectFile / DirectByteFile location (§10.6 / §10.11).
    pub loc: i64,
    pub maxloc: i64,
    pub locked: bool,
    /// Written direct images keyed by location (1-based).
    pub direct_images: std::collections::BTreeMap<i64, String>,
    /// DirectByteFile store (index 0 = location 1).
    pub byte_store: Vec<u8>,
    reader: Option<BufReader<OsFile>>,
    writer: Option<OsFile>,
    byte_reader: Option<BufReader<OsFile>>,
    byte_writer: Option<OsFile>,
}

impl FileHandle {
    pub fn new(filename: String, kind: FileKind) -> Self {
        let mode = if matches!(kind, FileKind::InFile | FileKind::InByteFile) {
            FileMode::In
        } else {
            FileMode::Out
        };
        Self {
            filename,
            mode,
            kind,
            open: false,
            image: TextFrame::notext(),
            endfile: true,
            access: AccessModes::defaults_for(mode),
            line: 1,
            page: 0,
            spacing: 1,
            lines_per_page: DEFAULT_LINES_PER_PAGE,
            default_lines_per_page: DEFAULT_LINES_PER_PAGE,
            loc: 0,
            maxloc: 0,
            locked: false,
            direct_images: std::collections::BTreeMap::new(),
            byte_store: Vec::new(),
            reader: None,
            writer: None,
            byte_reader: None,
            byte_writer: None,
        }
    }
}

/// Per-program BASICIO file registry (interpreter).
#[derive(Debug, Default)]
pub struct BasicioState {
    pub files: HashMap<u64, FileHandle>,
    pub sysin_id: u64,
    pub sysout_id: u64,
    /// §10 `terminate_program` request.
    pub terminate_requested: bool,
    pub input_linelength: i64,
    pub output_linelength: i64,
    initialized: bool,
}

impl BasicioState {
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
            sysin_id: SYSIN_IDENTITY,
            sysout_id: SYSOUT_IDENTITY,
            terminate_requested: false,
            input_linelength: DEFAULT_INPUT_LINELENGTH,
            output_linelength: DEFAULT_OUTPUT_LINELENGTH,
            initialized: false,
        }
    }

    /// Ensure terminal SysIn / SysOut file objects exist in `files`.
    pub fn ensure_terminals(&mut self) {
        if self.initialized {
            return;
        }
        self.files.insert(
            self.sysin_id,
            FileHandle::new("<SYSIN>".into(), FileKind::InFile),
        );
        self.files.insert(
            self.sysout_id,
            FileHandle::new("<SYSOUT>".into(), FileKind::PrintFile),
        );
        if let Some(f) = self.files.get_mut(&self.sysin_id) {
            f.open = true;
            f.endfile = false;
            // §10: SYSIN.open(blanks(INPUT_LINELENGTH))
            if let Ok(image) = TextFrame::blanks(self.input_linelength) {
                f.image = image;
                f.image.setpos(f.image.length + 1);
            }
        }
        if let Some(f) = self.files.get_mut(&self.sysout_id) {
            f.open = true;
            f.endfile = false;
            // §10: SYSOUT.open(blanks(OUTPUT_LINELENGTH))
            if let Ok(image) = TextFrame::blanks(self.output_linelength) {
                f.image = image;
                f.image.setpos(1);
            }
            f.kind = FileKind::PrintFile;
            f.page = 0;
            f.line = 1;
            f.spacing = 1;
        }
        self.initialized = true;
    }
}

fn empty_block() -> Block {
    Block {
        prefix: None,
        name: String::new(),
        directives: Vec::new(),
        externals: Vec::new(),
        declarations: Vec::new(),
        arrays: Vec::new(),
        switches: Vec::new(),
        procedures: Vec::new(),
        classes: Vec::new(),
        statements: Vec::new(),
        body: Vec::new(),
    }
}

fn filename_param() -> FormalParameter {
    FormalParameter {
        name: "FILENAME".into(),
        ty: Type::Text,
        mode: ParamMode::Value,
        mode_explicit: true,
        is_procedure: false,
        is_label: false,
        is_switch: false,
        span: 0..0,
    }
}

fn stub_class(
    name: &str,
    prefix: Option<&str>,
    parameters: Vec<FormalParameter>,
) -> ClassDeclaration {
    let specifications = parameters
        .iter()
        .map(|p| Specification {
            specifier: Specifier::Type(p.ty.clone()),
            names: vec![p.name.clone()],
        })
        .collect();
    ClassDeclaration {
        prefix: prefix.map(str::to_string),
        name: name.into(),
        parameters,
        specifications,
        virtual_part: Vec::new(),
        protection_part: Vec::new(),
        protection_map: Default::default(),
        body: empty_block(),
        has_inner: false,
        inner_label: None,
        tail_statements: Vec::new(),
        identifier_substitutions: std::collections::BTreeMap::new(),
        span: 0..0,
    }
}

pub fn file_system_class() -> ClassDeclaration {
    stub_class("File", None, vec![filename_param()])
}

pub fn image_file_system_class() -> ClassDeclaration {
    stub_class("ImageFile", Some("File"), Vec::new())
}

pub fn in_file_system_class() -> ClassDeclaration {
    stub_class("InFile", Some("ImageFile"), Vec::new())
}

pub fn out_file_system_class() -> ClassDeclaration {
    stub_class("OutFile", Some("ImageFile"), Vec::new())
}

pub fn print_file_system_class() -> ClassDeclaration {
    stub_class("PrintFile", Some("OutFile"), Vec::new())
}

pub fn byte_file_system_class() -> ClassDeclaration {
    stub_class("ByteFile", Some("File"), Vec::new())
}

pub fn in_byte_file_system_class() -> ClassDeclaration {
    stub_class("InByteFile", Some("ByteFile"), Vec::new())
}

pub fn out_byte_file_system_class() -> ClassDeclaration {
    stub_class("OutByteFile", Some("ByteFile"), Vec::new())
}

pub fn direct_file_system_class() -> ClassDeclaration {
    stub_class("DirectFile", Some("ImageFile"), Vec::new())
}

pub fn direct_byte_file_system_class() -> ClassDeclaration {
    stub_class("DirectByteFile", Some("ByteFile"), Vec::new())
}

/// Unmerged BASICIO stubs (for `raw_classes` / MIR layout input).
pub fn inject_system_class_stubs(classes: &mut HashMap<String, ClassDeclaration>) {
    let stubs = [
        file_system_class(),
        image_file_system_class(),
        in_file_system_class(),
        out_file_system_class(),
        print_file_system_class(),
        direct_file_system_class(),
        byte_file_system_class(),
        in_byte_file_system_class(),
        out_byte_file_system_class(),
        direct_byte_file_system_class(),
    ];
    for stub in stubs {
        if !classes.keys().any(|k| k.eq_ignore_ascii_case(&stub.name)) {
            classes.insert(stub.name.clone(), stub);
        }
    }
}

const BASICIO_CLASS_NAMES: &[&str] = &[
    "File",
    "ImageFile",
    "InFile",
    "OutFile",
    "PrintFile",
    "DirectFile",
    "ByteFile",
    "InByteFile",
    "OutByteFile",
    "DirectByteFile",
];

/// Inject BASICIO system classes in concatenated form unless already present.
pub fn inject_system_classes(classes: &mut HashMap<String, ClassDeclaration>) {
    let already = BASICIO_CLASS_NAMES
        .iter()
        .any(|name| classes.keys().any(|k| k.eq_ignore_ascii_case(name)));
    if already {
        if classes
            .get("OutFile")
            .or_else(|| {
                classes
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("OutFile"))
                    .map(|(_, v)| v)
            })
            .is_some_and(|c| c.parameters.is_empty())
        {
            let to_merge: Vec<ClassDeclaration> = BASICIO_CLASS_NAMES
                .iter()
                .filter_map(|name| {
                    classes
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case(name))
                        .map(|(_, c)| c.clone())
                })
                .collect();
            if let Ok(merged) =
                crate::concatenate::concatenate_classes_with_externals(&to_merge, &HashMap::new())
            {
                for (name, class) in merged {
                    classes.insert(name, class);
                }
            }
        }
        return;
    }
    inject_system_class_stubs(classes);
    let to_merge: Vec<ClassDeclaration> = BASICIO_CLASS_NAMES
        .iter()
        .filter_map(|name| {
            classes
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, c)| c.clone())
        })
        .collect();
    if let Ok(merged) =
        crate::concatenate::concatenate_classes_with_externals(&to_merge, &HashMap::new())
    {
        for (name, class) in merged {
            classes.insert(name, class);
        }
    }
}

pub fn is_basicio_class(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "file"
            | "imagefile"
            | "infile"
            | "outfile"
            | "printfile"
            | "directfile"
            | "bytefile"
            | "inbytefile"
            | "outbytefile"
            | "directbytefile"
    )
}

/// Whether a connected BASICIO class should absorb a bare free-procedure name.
/// Input-only names skip OutFile/PrintFile; output-only names skip InFile.
pub fn basicio_class_supports_free_method(class_name: &str, method: &str) -> bool {
    if !is_basicio_class(class_name) {
        return false;
    }
    let class = class_name.to_ascii_lowercase();
    let method = method.to_ascii_lowercase();
    match method.as_str() {
        "inimage" | "inchar" | "intext" | "inint" | "inreal" | "infrac" | "inrecord"
        | "lastitem" | "endfile" => {
            matches!(
                class.as_str(),
                "infile" | "imagefile" | "directfile" | "file"
            )
        }
        "outtext" | "outimage" | "outchar" | "outint" | "outfix" | "outreal" | "outfrac"
        | "outrecord" | "breakoutimage" | "field" | "eject" | "spacing" | "linesperpage"
        | "line" | "page" | "checkpoint" => {
            matches!(
                class.as_str(),
                "outfile" | "printfile" | "imagefile" | "directfile" | "file"
            )
        }
        _ => true, // open/close/setpos/image/… shared ImageFile attributes
    }
}

pub fn is_basicio_method(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "open"
            | "close"
            | "isopen"
            | "setaccess"
            | "filename"
            | "image"
            | "setpos"
            | "pos"
            | "more"
            | "length"
            | "outtext"
            | "outchar"
            | "outimage"
            | "outrecord"
            | "breakoutimage"
            | "checkpoint"
            | "inimage"
            | "inrecord"
            | "inchar"
            | "endfile"
            | "lastitem"
            | "intext"
            | "inint"
            | "inreal"
            | "infrac"
            | "outint"
            | "outfix"
            | "outreal"
            | "outfrac"
            | "field"
            | "line"
            | "page"
            | "linesperpage"
            | "spacing"
            | "eject"
            | "location"
            | "lastloc"
            | "maxloc"
            | "locate"
            | "deleteimage"
            | "locked"
            | "lock"
            | "unlock"
            | "bytesize"
            | "inbyte"
            | "outbyte"
    )
}

pub fn is_basicio_free_procedure(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "outchar"
            | "breakoutimage"
            | "inimage"
            | "inchar"
            | "endfile"
            | "sysin"
            | "sysout"
            | "outtext"
            | "outimage"
            | "outint"
            | "inline"
            | "terminate_program"
    ) || free_basicio_target(name).is_some()
}

/// Terminal target for free identifiers under the Standard embedding
/// `inspect SYSIN do inspect SYSOUT do` (SYSOUT is innermost).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreeBasicioTarget {
    SysIn,
    SysOut,
}

/// Map a free BASICIO attribute/procedure name to SYSIN or SYSOUT.
/// Shared names (`open`, `pos`, `image`, …) resolve to SYSOUT (innermost).
pub fn free_basicio_target(name: &str) -> Option<FreeBasicioTarget> {
    match name.to_ascii_lowercase().as_str() {
        "inimage" | "inchar" | "intext" | "inint" | "inreal" | "infrac" | "inrecord"
        | "lastitem" | "endfile" => Some(FreeBasicioTarget::SysIn),
        "outtext" | "outimage" | "outchar" | "outint" | "outfix" | "outreal" | "outfrac"
        | "outrecord" | "breakoutimage" | "field" | "eject" | "spacing" | "linesperpage"
        | "line" | "page" | "open" | "close" | "isopen" | "filename" | "image" | "setpos"
        | "pos" | "more" | "length" | "setaccess" | "checkpoint" => Some(FreeBasicioTarget::SysOut),
        _ => None,
    }
}

/// Result type for a BASICIO method used in expression position.
pub fn basicio_method_result_type(name: &str) -> Type {
    match name.to_ascii_lowercase().as_str() {
        "isopen" | "endfile" | "open" | "close" | "setaccess" | "more" | "checkpoint"
        | "inrecord" | "lastitem" | "deleteimage" | "locked" | "unlock" => Type::Boolean,
        "inchar" => Type::Character,
        "filename" | "image" | "intext" | "field" => Type::Text,
        "pos" | "length" | "inint" | "infrac" | "line" | "page" | "linesperpage" | "location"
        | "lastloc" | "maxloc" | "lock" | "bytesize" | "inbyte" => Type::Integer { short: false },
        "inreal" => Type::Real { long: true },
        _ => Type::Integer { short: false },
    }
}

pub fn basicio_free_result_type(name: &str) -> Option<Type> {
    match name.to_ascii_lowercase().as_str() {
        "inchar" => Some(Type::Character),
        "endfile" => Some(Type::Boolean),
        "sysin" => Some(Type::ObjectRef("InFile".into())),
        "sysout" => Some(Type::ObjectRef("PrintFile".into())),
        "inline" => Some(Type::Text),
        "line" | "page" | "pos" | "length" | "inint" | "infrac" | "linesperpage" => {
            Some(Type::Integer { short: false })
        }
        "lastitem" | "more" | "isopen" | "open" | "close" | "checkpoint" => Some(Type::Boolean),
        "filename" | "image" | "intext" | "field" => Some(Type::Text),
        "inreal" => Some(Type::Real { long: true }),
        "inimage" | "outchar" | "breakoutimage" | "outtext" | "outimage" | "outint" | "eject"
        | "spacing" | "terminate_program" => None,
        _ => None,
    }
}

/// Register a newly constructed BASICIO file object.
pub fn register_file_object(
    state: &mut BasicioState,
    identity: u64,
    class_name: &str,
    filename: String,
) -> Result<(), String> {
    if filename.is_empty() {
        return Err("file: FILENAME is notext".into());
    }
    let kind = FileKind::from_class_name(class_name);
    state
        .files
        .insert(identity, FileHandle::new(filename, kind));
    Ok(())
}

/// Apply one `setaccess` mode text (§10.1.1). Returns false if unrecognized.
pub fn set_access(handle: &mut FileHandle, mode: &str) -> bool {
    let m = mode.trim().to_ascii_lowercase();
    match m.as_str() {
        "shared" => handle.access.shared = true,
        "noshared" => handle.access.shared = false,
        "append" => handle.access.append = true,
        "noappend" => handle.access.append = false,
        "create" => handle.access.create = CreateMode::Create,
        "nocreate" => handle.access.create = CreateMode::NoCreate,
        "anycreate" => handle.access.create = CreateMode::AnyCreate,
        "readonly" => handle.access.readwrite = ReadWriteMode::ReadOnly,
        "writeonly" => handle.access.readwrite = ReadWriteMode::WriteOnly,
        "readwrite" => handle.access.readwrite = ReadWriteMode::ReadWrite,
        "rewind" => handle.access.rewind = true,
        "norewind" => handle.access.rewind = false,
        "purge" => handle.access.purge = true,
        "nopurge" => handle.access.purge = false,
        other if other.starts_with("bytesize:") => {
            let rest = &other["bytesize:".len()..];
            let Ok(n) = rest.parse::<u16>() else {
                return false;
            };
            handle.access.bytesize = if n == 0 { 8 } else { n };
        }
        _ => return false,
    }
    true
}

fn blank_fill_image(image: &mut TextFrame) -> Result<(), String> {
    if image.is_notext() {
        return Ok(());
    }
    image.assign_value_from(&TextFrame::notext())
}

/// Open an image file with the Standard `open(fileimage)` contract.
pub fn open_file(handle: &mut FileHandle, fileimage: TextFrame) -> Result<bool, String> {
    if handle.open {
        return Ok(false);
    }
    if handle.kind.is_byte() {
        return Err("open: bytefile open takes no fileimage".into());
    }
    if fileimage.is_notext() {
        return Err("open: fileimage is notext".into());
    }
    let path = Path::new(&handle.filename);
    let exists = path.exists();

    match handle.kind {
        FileKind::OutFile | FileKind::PrintFile => {
            match handle.access.create {
                CreateMode::Create if exists => return Ok(false),
                CreateMode::NoCreate if !exists => return Ok(false),
                _ => {}
            }
            let mut opts = OpenOptions::new();
            opts.create(true).write(true);
            if handle.access.append {
                opts.append(true);
            } else {
                opts.truncate(true);
            }
            let file = match opts.open(path) {
                Ok(f) => f,
                Err(_) => return Ok(false),
            };
            handle.writer = Some(file);
            handle.image = fileimage;
            blank_fill_image(&mut handle.image)?;
            handle.image.setpos(1);
            handle.open = true;
            handle.endfile = false;
            if handle.kind == FileKind::PrintFile {
                handle.page = 0;
                handle.line = 1;
                handle.spacing = 1;
                // §10.7.1: eject(1) after open
                file_eject(handle, 1)?;
            }
            Ok(true)
        }
        FileKind::InFile => {
            if !exists {
                return Ok(false);
            }
            let file = match OsFile::open(path) {
                Ok(f) => f,
                Err(_) => return Ok(false),
            };
            handle.reader = Some(BufReader::new(file));
            handle.image = fileimage;
            blank_fill_image(&mut handle.image)?;
            handle.image.setpos(handle.image.length + 1);
            handle.open = true;
            handle.endfile = false;
            Ok(true)
        }
        FileKind::DirectFile => {
            // A direct file is always opened for read+write and created on
            // demand, so the create-mode existence gate does not apply (same
            // as the native and wasm runtimes). CBL86 simtst81/simtst85 do
            // `setaccess("CREATE")` on direct files that already exist and
            // still expect `open` to succeed.
            // In-memory direct file backed by optional seed file (line-per-image).
            if exists {
                let contents = std::fs::read_to_string(path)
                    .map_err(|e| format!("directfile open read failed: {e}"))?;
                handle.direct_images.clear();
                for (i, line) in contents.lines().enumerate() {
                    handle
                        .direct_images
                        .insert((i as i64) + 1, line.to_string());
                }
            } else {
                handle.direct_images.clear();
            }
            handle.image = fileimage;
            blank_fill_image(&mut handle.image)?;
            handle.image.setpos(1);
            handle.maxloc = i64::MAX - 1;
            handle.loc = 1;
            handle.open = true;
            handle.endfile = false;
            handle.locked = false;
            Ok(true)
        }
        _ => Err("open: unexpected file kind for image open".into()),
    }
}

/// Bytefile / parameterless open (§10.9.1 / §10.10.1 / §10.11.1).
pub fn open_bytefile(handle: &mut FileHandle) -> Result<bool, String> {
    if handle.open {
        return Ok(false);
    }
    if !handle.kind.is_byte() {
        return Err("open: image file requires fileimage".into());
    }
    let path = Path::new(&handle.filename);
    let exists = path.exists();
    match handle.kind {
        FileKind::OutByteFile => {
            match handle.access.create {
                CreateMode::Create if exists => return Ok(false),
                CreateMode::NoCreate if !exists => return Ok(false),
                _ => {}
            }
            let mut opts = OpenOptions::new();
            opts.create(true).write(true);
            if handle.access.append {
                opts.append(true);
            } else {
                opts.truncate(true);
            }
            let file = match opts.open(path) {
                Ok(f) => f,
                Err(_) => return Ok(false),
            };
            handle.byte_writer = Some(file);
            handle.open = true;
            handle.endfile = false;
            Ok(true)
        }
        FileKind::InByteFile => {
            if !exists {
                return Ok(false);
            }
            let file = match OsFile::open(path) {
                Ok(f) => f,
                Err(_) => return Ok(false),
            };
            handle.byte_reader = Some(BufReader::new(file));
            handle.open = true;
            handle.endfile = false;
            Ok(true)
        }
        FileKind::DirectByteFile => {
            // As for DirectFile: read+write, created on demand, so create mode
            // does not gate the open.
            if exists {
                handle.byte_store =
                    std::fs::read(path).map_err(|e| format!("directbytefile open failed: {e}"))?;
            } else {
                handle.byte_store.clear();
            }
            handle.loc = 1;
            handle.maxloc = i64::MAX - 1;
            handle.open = true;
            handle.endfile = false;
            handle.locked = false;
            Ok(true)
        }
        _ => Err("open_bytefile: not a bytefile".into()),
    }
}

pub fn close_file(handle: &mut FileHandle) -> Result<bool, String> {
    if !handle.open {
        return Ok(false);
    }
    match handle.kind {
        FileKind::OutFile | FileKind::PrintFile => {
            if handle.image.pos != 1 {
                file_out_image(handle)?;
            }
            if handle.kind == FileKind::PrintFile {
                let lpp = handle.lines_per_page;
                let _ = file_eject(handle, lpp);
                handle.line = 0;
                handle.spacing = 1;
                handle.lines_per_page = handle.default_lines_per_page;
            }
        }
        FileKind::DirectFile => {
            if handle.locked {
                let _ = file_unlock(handle);
            }
            // Persist images as newline-separated records.
            let mut lines = Vec::new();
            let last = handle
                .direct_images
                .keys()
                .next_back()
                .copied()
                .unwrap_or(0);
            for loc in 1..=last {
                lines.push(handle.direct_images.get(&loc).cloned().unwrap_or_default());
            }
            let body = lines.join("\n");
            let body = if body.is_empty() {
                body
            } else {
                format!("{body}\n")
            };
            let _ = std::fs::write(&handle.filename, body);
            handle.loc = 0;
            handle.maxloc = 0;
            handle.direct_images.clear();
        }
        FileKind::DirectByteFile => {
            if handle.locked {
                let _ = file_unlock(handle);
            }
            let _ = std::fs::write(&handle.filename, &handle.byte_store);
            handle.maxloc = 0;
            handle.byte_store.clear();
        }
        FileKind::OutByteFile | FileKind::InByteFile => {}
        FileKind::InFile => {}
    }
    if let Some(mut writer) = handle.writer.take() {
        let _ = writer.flush();
    }
    if let Some(mut writer) = handle.byte_writer.take() {
        let _ = writer.flush();
    }
    handle.reader = None;
    handle.byte_reader = None;
    if handle.access.purge && handle.mode == FileMode::Out {
        let _ = std::fs::remove_file(&handle.filename);
    }
    handle.image = TextFrame::notext();
    handle.open = false;
    handle.endfile = true;
    Ok(true)
}

fn write_image_bytes(
    handle: &mut FileHandle,
    bytes: &[u8],
    add_newline: bool,
) -> Result<(), String> {
    let Some(writer) = handle.writer.as_mut() else {
        return Err("OutFile: no writer".into());
    };
    writer
        .write_all(bytes)
        .map_err(|e| format!("OutFile write failed: {e}"))?;
    if add_newline {
        writer
            .write_all(b"\n")
            .map_err(|e| format!("OutFile write failed: {e}"))?;
    }
    writer
        .flush()
        .map_err(|e| format!("OutFile flush failed: {e}"))?;
    Ok(())
}

pub fn file_out_image(handle: &mut FileHandle) -> Result<(), String> {
    if !handle.open {
        return Err("outimage: file is not open".into());
    }
    match handle.kind {
        FileKind::PrintFile => {
            if handle.line > handle.lines_per_page {
                file_eject(handle, 1)?;
            }
            let content = handle.image.content();
            write_image_bytes(handle, content.as_bytes(), true)?;
            handle.line += handle.spacing;
            blank_fill_image(&mut handle.image)?;
            handle.image.setpos(1);
            Ok(())
        }
        FileKind::DirectFile => {
            if handle.loc > handle.maxloc {
                return Err("outimage: file overflow".into());
            }
            handle
                .direct_images
                .insert(handle.loc, handle.image.content());
            file_locate(handle, handle.loc + 1)?;
            blank_fill_image(&mut handle.image)?;
            handle.image.setpos(1);
            Ok(())
        }
        FileKind::OutFile => {
            let content = handle.image.content();
            write_image_bytes(handle, content.as_bytes(), true)?;
            blank_fill_image(&mut handle.image)?;
            handle.image.setpos(1);
            Ok(())
        }
        // Native C writes through `fp` for any non-terminal open file. InFile
        // here has a reader only, so treat outimage as an image reset (no-op
        // write) rather than aborting — matches corpus programs that bind free
        // `outimage` under `inspect InFile` (simtst96).
        FileKind::InFile | FileKind::InByteFile => {
            blank_fill_image(&mut handle.image)?;
            handle.image.setpos(1);
            Ok(())
        }
        _ => Err("outimage: not an output image file".into()),
    }
}

pub fn file_out_record(handle: &mut FileHandle) -> Result<(), String> {
    if !handle.open {
        return Err("outrecord: file is not open".into());
    }
    let end = handle.image.pos.saturating_sub(1).max(0);
    let payload = if end == 0 || handle.image.is_notext() {
        String::new()
    } else {
        handle
            .image
            .subframe(1, end)
            .map(|f| f.content())
            .unwrap_or_default()
    };
    match handle.kind {
        FileKind::PrintFile => {
            if handle.line > handle.lines_per_page {
                file_eject(handle, 1)?;
            }
            write_image_bytes(handle, payload.as_bytes(), true)?;
            handle.line += handle.spacing;
            handle.image.setpos(1);
            Ok(())
        }
        FileKind::OutFile => {
            write_image_bytes(handle, payload.as_bytes(), true)?;
            handle.image.setpos(1);
            Ok(())
        }
        _ => Err("outrecord: not supported on this file kind".into()),
    }
}

pub fn file_break_out_image(handle: &mut FileHandle) -> Result<(), String> {
    if !handle.open {
        return Err("breakoutimage: file is not open".into());
    }
    let end = handle.image.pos.saturating_sub(1).max(0);
    let payload = if end == 0 || handle.image.is_notext() {
        String::new()
    } else {
        handle
            .image
            .subframe(1, end)
            .map(|f| f.content())
            .unwrap_or_default()
    };
    // Host files: still terminate the partial record with newline.
    // PrintFile: does not update LINE/PAGE (§10.7.5 note).
    write_image_bytes(handle, payload.as_bytes(), true)?;
    blank_fill_image(&mut handle.image)?;
    handle.image.setpos(1);
    Ok(())
}

pub fn file_in_image(handle: &mut FileHandle) -> Result<(), String> {
    if !handle.open {
        return Err("inimage: file is not open".into());
    }
    match handle.kind {
        FileKind::DirectFile => {
            handle.image.setpos(1);
            let last = file_lastloc(handle)?;
            handle.endfile = handle.loc > last;
            if handle.endfile {
                let em = TextFrame::from_mutable(&EM_CHAR.to_string());
                handle.image.assign_value_from(&em)?;
            } else if let Some(content) = handle.direct_images.get(&handle.loc).cloned() {
                let src = TextFrame::from_mutable(&content);
                handle.image.assign_value_from(&src)?;
                handle.image.setpos(1);
            } else {
                // Unwritten image: fill with NUL, pos = length+1
                while handle.image.more() {
                    handle.image.putchar('\0')?;
                }
            }
            file_locate(handle, handle.loc + 1)?;
            Ok(())
        }
        FileKind::InFile => {
            if handle.endfile {
                return Err("inimage: end of file".into());
            }
            let Some(reader) = handle.reader.as_mut() else {
                return Err("inimage: no reader".into());
            };
            let mut line = String::new();
            let n = reader
                .read_line(&mut line)
                .map_err(|e| format!("inimage read failed: {e}"))?;
            if n == 0 {
                handle.endfile = true;
                let em = TextFrame::from_mutable(&EM_CHAR.to_string());
                handle.image.assign_value_from(&em)?;
                handle.image.setpos(1);
                return Ok(());
            }
            while line.ends_with('\n') || line.ends_with('\r') {
                line.pop();
            }
            if line.chars().count() as i64 > handle.image.length {
                return Err("inimage: image too short for external image".into());
            }
            let src = TextFrame::from_mutable(&line);
            handle.image.assign_value_from(&src)?;
            handle.image.setpos(1);
            Ok(())
        }
        _ => Err("inimage: not an input image file".into()),
    }
}

/// §10.4.2 inrecord — no space-filling; returns true if truncated.
pub fn file_in_record(handle: &mut FileHandle) -> Result<bool, String> {
    if !handle.open || handle.endfile {
        return Err("inrecord: file closed or at endfile".into());
    }
    let Some(reader) = handle.reader.as_mut() else {
        return Err("inrecord: no reader".into());
    };
    let mut line = String::new();
    let n = reader
        .read_line(&mut line)
        .map_err(|e| format!("inrecord read failed: {e}"))?;
    if n == 0 {
        handle.endfile = true;
        handle.image.setpos(1);
        handle.image.putchar(EM_CHAR)?;
        return Ok(false);
    }
    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }
    let chars: Vec<char> = line.chars().collect();
    let capacity = handle.image.length as usize;
    let truncated = chars.len() > capacity;
    let take = capacity.min(chars.len());
    handle.image.setpos(1);
    for &ch in &chars[..take] {
        handle.image.putchar(ch)?;
    }
    // POS = transferred + 1
    Ok(truncated)
}

pub fn file_in_char(handle: &mut FileHandle) -> Result<char, String> {
    if !handle.open {
        return Err("inchar: file is not open".into());
    }
    if handle.kind == FileKind::DirectFile {
        while !handle.image.more() {
            file_in_image(handle)?;
        }
        return handle.image.getchar();
    }
    if !handle.image.more() {
        file_in_image(handle)?;
    }
    handle.image.getchar()
}

pub fn file_out_char(handle: &mut FileHandle, ch: char) -> Result<(), String> {
    if !handle.open {
        return Err("outchar: file is not open".into());
    }
    if !handle.image.more() {
        file_out_image(handle)?;
    }
    handle.image.putchar(ch)
}

pub fn file_out_text(handle: &mut FileHandle, text: &str) -> Result<(), String> {
    if !handle.open {
        return Err("outtext: file is not open".into());
    }
    if handle.kind.is_byte() {
        return file_byte_out_text(handle, text);
    }
    let t_len = text.chars().count() as i64;
    if handle.image.pos > 1 && t_len > handle.image.length - handle.image.pos + 1 {
        file_out_image(handle)?;
    }
    for ch in text.chars() {
        file_out_char(handle, ch)?;
    }
    Ok(())
}

pub fn file_checkpoint(handle: &mut FileHandle) -> bool {
    if !handle.open {
        return false;
    }
    if let Some(writer) = handle.writer.as_mut() {
        return writer.flush().is_ok();
    }
    if let Some(writer) = handle.byte_writer.as_mut() {
        return writer.flush().is_ok();
    }
    matches!(handle.kind, FileKind::DirectFile | FileKind::DirectByteFile)
}

// --- Phase 3: item-oriented I/O ---

pub fn file_lastitem(handle: &mut FileHandle) -> Result<bool, String> {
    let mut c = ' ';
    while !handle.endfile && (c == ' ' || c == '\t') {
        c = file_in_char(handle)?;
    }
    let at_end = handle.endfile;
    if c != ' ' && c != '\t' {
        handle.image.setpos(handle.image.pos - 1);
    }
    Ok(at_end)
}

pub fn file_in_text(handle: &mut FileHandle, w: i64) -> Result<TextFrame, String> {
    if w <= 0 {
        return Ok(TextFrame::notext());
    }
    let mut t = TextFrame::blanks(w)?;
    while t.more() {
        let ch = file_in_char(handle)?;
        t.putchar(ch)?;
    }
    Ok(t)
}

pub fn file_in_int(handle: &mut FileHandle) -> Result<i64, String> {
    if file_lastitem(handle)? {
        return Err("inint: end of file".into());
    }
    let mut t = handle
        .image
        .subframe(handle.image.pos, handle.image.length - handle.image.pos + 1)?;
    let value = t.deedit_getint()?;
    handle.image.setpos(handle.image.pos + t.pos - 1);
    Ok(value)
}

pub fn file_in_real(handle: &mut FileHandle) -> Result<f64, String> {
    if file_lastitem(handle)? {
        return Err("inreal: end of file".into());
    }
    let mut t = handle
        .image
        .subframe(handle.image.pos, handle.image.length - handle.image.pos + 1)?;
    let value = t.deedit_getreal()?;
    handle.image.setpos(handle.image.pos + t.pos - 1);
    Ok(value)
}

pub fn file_in_frac(handle: &mut FileHandle) -> Result<i64, String> {
    if file_lastitem(handle)? {
        return Err("infrac: end of file".into());
    }
    let mut t = handle
        .image
        .subframe(handle.image.pos, handle.image.length - handle.image.pos + 1)?;
    let value = t.deedit_getfrac()?;
    handle.image.setpos(handle.image.pos + t.pos - 1);
    Ok(value)
}

pub fn file_field(handle: &mut FileHandle, w: i64) -> Result<TextFrame, String> {
    if w > handle.image.length {
        return Err("FIELD: item too long".into());
    }
    if w <= 0 {
        return Err("FIELD: w must be positive".into());
    }
    if handle.image.pos + w - 1 > handle.image.length {
        file_out_image(handle)?;
    }
    let field = handle.image.subframe(handle.image.pos, w)?;
    handle.image.setpos(handle.image.pos + w);
    Ok(field)
}

fn exact_int_width(i: i64) -> i64 {
    format!("{i}").chars().count() as i64
}

fn exact_fix_width(r: f64, n: i64) -> i64 {
    let formatted = if n <= 0 {
        format!("{}", r.round() as i64)
    } else {
        format!("{:.*}", n as usize, r)
    };
    formatted.chars().count() as i64
}

pub fn file_out_int(handle: &mut FileHandle, i: i64, w: i64) -> Result<(), String> {
    if w == 0 {
        let width = exact_int_width(i);
        let mut field = file_field(handle, width)?;
        field.edit_putint(i)?;
    } else if w < 0 {
        let mut field = file_field(handle, -w)?;
        blank_fill_image(&mut field)?;
        let width = exact_int_width(i).min(-w);
        let mut left = field.subframe(1, width)?;
        left.edit_putint(i)?;
    } else {
        let mut field = file_field(handle, w)?;
        field.edit_putint(i)?;
    }
    Ok(())
}

/// Format an integer for free/`SYSOUT` `outint(i,w)` (§10.5.8 field rules).
pub fn format_outint_field(i: i64, w: i64) -> Result<String, String> {
    let digits = i.to_string();
    let n = digits.chars().count() as i64;
    if w == 0 {
        return Ok(digits);
    }
    let width = w.abs();
    if n > width {
        // Standard §10.5.8 / putint: overlong items are asterisk-filled.
        return Ok("*".repeat(width as usize));
    }
    if w > 0 {
        Ok(format!("{digits:>width$}", width = width as usize))
    } else {
        Ok(format!("{digits:<width$}", width = width as usize))
    }
}

/// Format a real for free/`SYSOUT` `outreal(r,n,w)` via a temporary text frame.
pub fn format_outreal_field(r: f64, n: i64, w: i64) -> Result<String, String> {
    let width = if w == 0 {
        let mut tmp = TextFrame::blanks(64)?;
        tmp.edit_putreal(r, n)?;
        tmp.content().trim().chars().count().max(1) as i64
    } else {
        w.abs()
    };
    let mut field = TextFrame::blanks(width)?;
    field.edit_putreal(r, n)?;
    Ok(field.content().to_string())
}

/// Format a real for free/`SYSOUT` `outfix(r,n,w)`.
pub fn format_outfix_field(r: f64, n: i64, w: i64) -> Result<String, String> {
    let width = if w == 0 {
        exact_fix_width(r, n)
    } else {
        w.abs()
    };
    let mut field = TextFrame::blanks(width)?;
    field.edit_putfix(r, n)?;
    Ok(field.content().to_string())
}

pub fn file_out_fix(handle: &mut FileHandle, r: f64, n: i64, w: i64) -> Result<(), String> {
    if w == 0 {
        let width = exact_fix_width(r, n);
        let mut field = file_field(handle, width)?;
        field.edit_putfix(r, n)?;
    } else if w < 0 {
        let mut field = file_field(handle, -w)?;
        blank_fill_image(&mut field)?;
        let width = exact_fix_width(r, n).min(-w);
        let mut left = field.subframe(1, width)?;
        left.edit_putfix(r, n)?;
    } else {
        let mut field = file_field(handle, w)?;
        field.edit_putfix(r, n)?;
    }
    Ok(())
}

pub fn file_out_real(handle: &mut FileHandle, r: f64, n: i64, w: i64) -> Result<(), String> {
    // Approximate exact width via a temporary blanks frame.
    let width_needed = {
        let mut tmp = TextFrame::blanks(64)?;
        tmp.edit_putreal(r, n)?;
        tmp.content().trim().chars().count() as i64
    };
    if w == 0 {
        let mut field = file_field(handle, width_needed.max(1))?;
        field.edit_putreal(r, n)?;
    } else if w < 0 {
        let mut field = file_field(handle, -w)?;
        blank_fill_image(&mut field)?;
        let width = width_needed.min(-w).max(1);
        let mut left = field.subframe(1, width)?;
        left.edit_putreal(r, n)?;
    } else {
        let mut field = file_field(handle, w)?;
        field.edit_putreal(r, n)?;
    }
    Ok(())
}

pub fn file_out_frac(handle: &mut FileHandle, i: i64, n: i64, w: i64) -> Result<(), String> {
    let width_needed = {
        let mut tmp = TextFrame::blanks(64)?;
        tmp.edit_putfrac(i, n)?;
        tmp.content().trim().chars().count() as i64
    };
    if w == 0 {
        let mut field = file_field(handle, width_needed.max(1))?;
        field.edit_putfrac(i, n)?;
    } else if w < 0 {
        let mut field = file_field(handle, -w)?;
        blank_fill_image(&mut field)?;
        let width = width_needed.min(-w).max(1);
        let mut left = field.subframe(1, width)?;
        left.edit_putfrac(i, n)?;
    } else {
        let mut field = file_field(handle, w)?;
        field.edit_putfrac(i, n)?;
    }
    Ok(())
}

// --- Phase 4: PrintFile ---

pub fn file_linesperpage(handle: &mut FileHandle, n: i64) -> i64 {
    let prev = handle.lines_per_page;
    handle.lines_per_page = if n > 0 {
        n
    } else if n < 0 {
        i64::MAX
    } else {
        handle.default_lines_per_page
    };
    prev
}

pub fn file_spacing(handle: &mut FileHandle, n: i64) -> Result<(), String> {
    if n < 0 || n > handle.lines_per_page {
        return Err("spacing: parameter out of range".into());
    }
    handle.spacing = n;
    Ok(())
}

pub fn file_eject(handle: &mut FileHandle, mut n: i64) -> Result<(), String> {
    if !handle.open {
        return Err("eject: file is not open".into());
    }
    if n <= 0 {
        return Err("eject: parameter out of range".into());
    }
    if n > handle.lines_per_page {
        n = 1;
    }
    if n <= handle.line {
        // Form feed / new page marker on host stream.
        if let Some(writer) = handle.writer.as_mut() {
            let _ = writeln!(writer);
            let _ = writer.flush();
        }
        handle.page += 1;
    }
    handle.line = n;
    Ok(())
}

// --- Phase 5: Direct + Byte ---

pub fn file_locate(handle: &mut FileHandle, i: i64) -> Result<(), String> {
    if i < 1 || i > handle.maxloc {
        return Err("locate: parameter out of range".into());
    }
    handle.loc = i;
    Ok(())
}

pub fn file_lastloc(handle: &FileHandle) -> Result<i64, String> {
    if !handle.open {
        return Err("lastloc: file closed".into());
    }
    match handle.kind {
        FileKind::DirectFile => Ok(handle
            .direct_images
            .keys()
            .next_back()
            .copied()
            .unwrap_or(0)),
        FileKind::DirectByteFile => Ok(handle.byte_store.len() as i64),
        _ => Err("lastloc: not a direct file".into()),
    }
}

pub fn file_maxloc(handle: &FileHandle) -> Result<i64, String> {
    if !handle.open {
        return Err("maxloc: file closed".into());
    }
    Ok(handle.maxloc)
}

pub fn file_delete_image(handle: &mut FileHandle) -> Result<bool, String> {
    if !handle.open || handle.kind != FileKind::DirectFile {
        return Ok(false);
    }
    if handle.direct_images.remove(&handle.loc).is_some() {
        file_locate(handle, handle.loc + 1)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub fn file_lock(handle: &mut FileHandle, timelimit: f64, _loc1: i64, _loc2: i64) -> i64 {
    if timelimit <= 0.0 {
        return -1;
    }
    if handle.locked {
        let _ = file_unlock(handle);
    }
    // Host stub: always succeeds immediately.
    handle.locked = true;
    0
}

pub fn file_unlock(handle: &mut FileHandle) -> bool {
    let ok = file_checkpoint(handle);
    if handle.locked {
        handle.locked = false;
    }
    ok
}

pub fn file_in_byte(handle: &mut FileHandle) -> Result<i64, String> {
    match handle.kind {
        FileKind::InByteFile => {
            if handle.endfile {
                return Err("inbyte: end of file".into());
            }
            let Some(reader) = handle.byte_reader.as_mut() else {
                return Err("inbyte: no reader".into());
            };
            let mut buf = [0u8; 1];
            use std::io::Read;
            match reader.read(&mut buf) {
                Ok(0) => {
                    handle.endfile = true;
                    Ok(0)
                }
                Ok(_) => Ok(buf[0] as i64),
                Err(e) => Err(format!("inbyte: {e}")),
            }
        }
        FileKind::DirectByteFile => {
            if !handle.open {
                return Err("inbyte: file closed".into());
            }
            let last = file_lastloc(handle)?;
            if handle.loc <= last {
                let idx = (handle.loc - 1) as usize;
                let b = handle.byte_store.get(idx).copied().unwrap_or(0);
                handle.loc += 1;
                Ok(b as i64)
            } else {
                Ok(0)
            }
        }
        _ => Err("inbyte: not a bytefile".into()),
    }
}

pub fn file_out_byte(handle: &mut FileHandle, x: i64) -> Result<(), String> {
    let max = (1i64 << handle.access.bytesize).saturating_sub(1);
    if x < 0 || x > max {
        return Err("outbyte: illegal byte value".into());
    }
    match handle.kind {
        FileKind::OutByteFile => {
            if !handle.open {
                return Err("outbyte: file closed".into());
            }
            let Some(writer) = handle.byte_writer.as_mut() else {
                return Err("outbyte: no writer".into());
            };
            writer
                .write_all(&[x as u8])
                .map_err(|e| format!("outbyte: {e}"))?;
            Ok(())
        }
        FileKind::DirectByteFile => {
            if !handle.open {
                return Err("outbyte: file closed".into());
            }
            if handle.loc > handle.maxloc {
                return Err("outbyte: file overflow".into());
            }
            let idx = (handle.loc - 1) as usize;
            if idx >= handle.byte_store.len() {
                handle.byte_store.resize(idx + 1, 0);
            }
            handle.byte_store[idx] = x as u8;
            handle.loc += 1;
            Ok(())
        }
        _ => Err("outbyte: not a bytefile".into()),
    }
}

pub fn file_byte_in_text(handle: &mut FileHandle, mut t: TextFrame) -> Result<TextFrame, String> {
    t.setpos(1);
    while t.more() && !handle.endfile {
        let b = file_in_byte(handle)?;
        if handle.endfile && b == 0 {
            break;
        }
        t.putchar(char::from_u32(b as u32).unwrap_or('\0'))?;
    }
    if handle.endfile {
        t.setpos(t.pos - 1);
    }
    t.subframe(1, t.pos - 1)
}

pub fn file_byte_out_text(handle: &mut FileHandle, text: &str) -> Result<(), String> {
    for ch in text.chars() {
        file_out_byte(handle, ch as u32 as i64)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_basicio_classes_with_filename() {
        let mut map = HashMap::new();
        inject_system_classes(&mut map);
        let file = map.get("File").expect("File");
        assert_eq!(file.parameters.len(), 1);
        assert!(file.parameters[0].name.eq_ignore_ascii_case("FILENAME"));
        let outfile = map.get("OutFile").expect("OutFile");
        assert!(
            outfile
                .parameters
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case("FILENAME")),
            "OutFile should inherit FILENAME, got {:?}",
            outfile
                .parameters
                .iter()
                .map(|p| &p.name)
                .collect::<Vec<_>>()
        );
        assert!(map.contains_key("InFile"));
        assert!(map.contains_key("PrintFile"));
        assert!(map.contains_key("DirectFile"));
        assert!(map.contains_key("InByteFile"));
    }

    #[test]
    fn set_access_recognizes_modes() {
        let mut h = FileHandle::new("x".into(), FileKind::OutFile);
        assert!(set_access(&mut h, "append"));
        assert!(h.access.append);
        assert!(!set_access(&mut h, "not-a-mode"));
    }
}
