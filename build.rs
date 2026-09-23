//! Build step.
//!
//! Two jobs, neither of which needs a crate dependency.
//!
//! Shaders under src/render/shaders are compiled to SPIR-V inside OUT_DIR. The
//! compiler is looked up in this order:
//!   1. RXSCOPE_GLSLC environment variable (full path to glslc or glslang);
//!   2. glslc / glslangValidator in PATH;
//!   3. %VULKAN_SDK%\Bin.
//!
//! The application icon is packed into a resource file and handed to the
//! linker. The resource format is written here rather than delegated to rc.exe
//! or to a build crate: it is a flat sequence of fixed headers, and link.exe
//! accepts the result as a plain input, so the whole path costs one function
//! and no tool that has to be found first.
//!
//! Neither the Vulkan SDK nor the icon is required to produce a working
//! binary in the icon case: a missing icon is reported and skipped, because an
//! executable that refuses to build over a picture is worse than one that
//! carries the default.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Icon used when the environment names none.
const ICON_PATH: &str = "assets/rxscope.ico";

/// Resource identifier of the icon group.
///
/// The shell picks the lowest numbered group for the file icon, and the window
/// code loads this same value, so the two have to agree; one is the
/// conventional choice and leaves room below nothing.
const ICON_GROUP_ID: u16 = 1;

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Glslc,
    Glslang,
}

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set"));
    println!("cargo:rerun-if-changed=build.rs");

    compile_shaders(&out_dir);
    embed_icon(&out_dir);
}

// ---------------------------------------------------------------- shaders

fn compile_shaders(out_dir: &Path) {
    let shader_dir = Path::new("src/render/shaders");
    println!("cargo:rerun-if-env-changed=RXSCOPE_GLSLC");
    println!("cargo:rerun-if-changed={}", shader_dir.display());

    let (compiler, kind) = find_compiler();

    let entries = std::fs::read_dir(shader_dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {}", shader_dir.display(), e));

    let mut compiled = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = match path.extension().and_then(|e| e.to_str()) {
            Some(e) => e,
            None => continue,
        };
        if !matches!(ext, "vert" | "frag" | "comp") {
            continue;
        }
        println!("cargo:rerun-if-changed={}", path.display());

        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let out = out_dir.join(format!("{}.spv", name));

        // Debug builds keep the SPIR-V unoptimized so a validation message
        // points at a readable instruction; release strips debug info.
        let optimize = std::env::var("PROFILE").map(|p| p == "release").unwrap_or(false);

        let status = match kind {
            Kind::Glslc => {
                let mut cmd = Command::new(&compiler);
                cmd.arg("--target-env=vulkan1.1").arg(&path).arg("-o").arg(&out);
                if optimize {
                    cmd.arg("-O");
                } else {
                    cmd.arg("-g").arg("-O0");
                }
                cmd.status()
            }
            Kind::Glslang => {
                let mut cmd = Command::new(&compiler);
                cmd.arg("-V").arg("--target-env").arg("vulkan1.1");
                if !optimize {
                    cmd.arg("-g");
                }
                cmd.arg(&path).arg("-o").arg(&out);
                cmd.status()
            }
        };

        match status {
            Ok(s) if s.success() => compiled += 1,
            Ok(s) => panic!("shader compilation failed for {} (exit {:?})", name, s.code()),
            Err(e) => panic!("cannot run {}: {}", compiler.display(), e),
        }
    }

    if compiled == 0 {
        panic!("no shaders found in {}", shader_dir.display());
    }
}

fn find_compiler() -> (PathBuf, Kind) {
    if let Ok(explicit) = std::env::var("RXSCOPE_GLSLC") {
        let p = PathBuf::from(explicit);
        let kind = classify(&p);
        if probe(&p, kind) {
            return (p, kind);
        }
        panic!("RXSCOPE_GLSLC points at {} which does not run", p.display());
    }

    // PATH lookup first, then the SDK layout.
    for (name, kind) in [("glslc", Kind::Glslc), ("glslangValidator", Kind::Glslang)] {
        let p = PathBuf::from(name);
        if probe(&p, kind) {
            return (p, kind);
        }
    }

    if let Ok(sdk) = std::env::var("VULKAN_SDK") {
        for (name, kind) in [("glslc.exe", Kind::Glslc), ("glslangValidator.exe", Kind::Glslang)] {
            let p = Path::new(&sdk).join("Bin").join(name);
            if p.exists() && probe(&p, kind) {
                return (p, kind);
            }
        }
    }

    panic!(
        "no GLSL compiler found. Install the Vulkan SDK or set RXSCOPE_GLSLC \
         to the full path of glslc.exe or glslangValidator.exe"
    );
}

fn classify(p: &Path) -> Kind {
    let name = p.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
    if name.contains("glslangvalidator") {
        Kind::Glslang
    } else {
        Kind::Glslc
    }
}

fn probe(p: &Path, kind: Kind) -> bool {
    let arg = match kind {
        Kind::Glslc => "--version",
        Kind::Glslang => "-v",
    };
    Command::new(p)
        .arg(arg)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ------------------------------------------------------------------- icon

fn embed_icon(out_dir: &Path) {
    println!("cargo:rerun-if-env-changed=RXSCOPE_ICON");

    // Resources are a Windows notion. Another target would reject the linker
    // argument outright, so the whole step is skipped rather than guarded at
    // the end.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let path = PathBuf::from(
        std::env::var("RXSCOPE_ICON").unwrap_or_else(|_| ICON_PATH.to_string()),
    );
    println!("cargo:rerun-if-changed={}", path.display());

    if !path.is_file() {
        println!(
            "cargo:warning=no icon at {}, the executable keeps the system default",
            path.display()
        );
        return;
    }

    let ico = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => {
            println!("cargo:warning=cannot read {}: {}", path.display(), e);
            return;
        }
    };

    let res = match build_resource(&ico) {
        Ok(bytes) => bytes,
        Err(reason) => {
            println!("cargo:warning={}: {}", path.display(), reason);
            return;
        }
    };

    let res_path = out_dir.join("rxscope.res");
    if let Err(e) = std::fs::write(&res_path, &res) {
        println!("cargo:warning=cannot write {}: {}", res_path.display(), e);
        return;
    }

    // The Microsoft linker recognizes a resource file by its extension and
    // converts it itself. The GNU one does not, so the same bytes are turned
    // into an object first.
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let linkable = if env == "gnu" {
        match convert_resource(out_dir, &res_path) {
            Some(object) => object,
            None => return,
        }
    } else {
        res_path
    };

    // Binaries only. A test harness links its own executable and has no use
    // for a shell icon, and the argument would be applied to every one of them.
    println!("cargo:rustc-link-arg-bins={}", linkable.display());
}

/// Turns a resource file into a linkable object for the GNU toolchain.
fn convert_resource(out_dir: &Path, res_path: &Path) -> Option<PathBuf> {
    let object = out_dir.join("rxscope-icon.o");
    let status = Command::new("windres")
        .arg("-J")
        .arg("res")
        .arg("-O")
        .arg("coff")
        .arg("-i")
        .arg(res_path)
        .arg("-o")
        .arg(&object)
        .status();

    match status {
        Ok(s) if s.success() => Some(object),
        Ok(s) => {
            println!("cargo:warning=windres failed (exit {:?}), icon skipped", s.code());
            None
        }
        Err(_) => {
            println!("cargo:warning=windres not found, icon skipped");
            None
        }
    }
}

/// Packs an icon file into a resource file.
///
/// Each image of the icon becomes one RT_ICON entry, and the directory is
/// rewritten as an RT_GROUP_ICON entry in which every file offset is replaced
/// by the identifier of the entry that now holds those bytes. That substitution
/// is the whole difference between the two formats; the image data is copied
/// verbatim, so a PNG compressed entry needs no separate path.
fn build_resource(ico: &[u8]) -> std::result::Result<Vec<u8>, String> {
    if ico.len() < 6 {
        return Err("shorter than an icon directory".to_string());
    }
    if u16le(ico, 0) != 0 || u16le(ico, 2) != 1 {
        return Err("not an icon file".to_string());
    }
    let count = u16le(ico, 4) as usize;
    if count == 0 {
        return Err("holds no images".to_string());
    }
    if ico.len() < 6 + count * 16 {
        return Err("directory is truncated".to_string());
    }

    let mut group = Vec::with_capacity(6 + count * 14);
    group.extend_from_slice(&0u16.to_le_bytes());
    group.extend_from_slice(&1u16.to_le_bytes());
    group.extend_from_slice(&(count as u16).to_le_bytes());

    let mut out = Vec::with_capacity(ico.len() + count * 64 + 128);
    // A resource file opens with an empty entry, which is what marks it as the
    // thirty two bit form.
    push_entry(&mut out, 0, 0, &[]);

    for index in 0..count {
        let at = 6 + index * 16;
        let width = ico[at];
        let height = ico[at + 1];
        let colours = ico[at + 2];
        let reserved = ico[at + 3];
        let bytes = u32le(ico, at + 8) as usize;
        let offset = u32le(ico, at + 12) as usize;

        let end = match offset.checked_add(bytes) {
            Some(e) if e <= ico.len() => e,
            _ => return Err(format!("image {} leaves the file", index)),
        };
        let data = &ico[offset..end];

        // Several authoring tools leave the plane and bit counts at nought in
        // the directory while the image itself states them. The loader picks
        // which image to use from these fields, so a nought there makes it pick
        // by nothing.
        let (planes, bits) = geometry(data, u16le(ico, at + 4), u16le(ico, at + 6));

        // Identifiers start at one because nought is not a valid resource
        // identifier, and RT_ICON has a numbering space of its own, so the
        // overlap with the group identifier is not a collision.
        let id = (index + 1) as u16;

        group.push(width);
        group.push(height);
        group.push(colours);
        group.push(reserved);
        group.extend_from_slice(&planes.to_le_bytes());
        group.extend_from_slice(&bits.to_le_bytes());
        group.extend_from_slice(&(bytes as u32).to_le_bytes());
        group.extend_from_slice(&id.to_le_bytes());

        push_entry(&mut out, RT_ICON, id, data);
    }

    push_entry(&mut out, RT_GROUP_ICON, ICON_GROUP_ID, &group);
    Ok(out)
}

/// Plane and bit counts of one image, corrected from the image when the
/// directory does not state them.
fn geometry(data: &[u8], planes: u16, bits: u16) -> (u16, u16) {
    if planes != 0 && bits != 0 {
        return (planes, bits);
    }
    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if data.len() >= 8 && data[..8] == PNG {
        // A compressed entry carries no header the directory can quote, and
        // every one of them is written as full colour.
        return (1, 32);
    }
    // The two fields sit at the same offsets in every bitmap header revision,
    // so the size only has to be plausible rather than exact.
    if data.len() >= 16 && u32le(data, 0) >= 40 {
        let planes = u16le(data, 12);
        let bits = u16le(data, 14);
        if planes != 0 && bits != 0 {
            return (planes, bits);
        }
    }
    (1, 32)
}

/// Appends one resource entry.
///
/// The header is fixed at thirty two bytes because both the type and the name
/// are written as ordinals, which occupy four bytes each and leave the header
/// aligned without padding. The data is padded so the next entry starts
/// aligned as well, which is what the converter expects.
fn push_entry(out: &mut Vec<u8>, kind: u16, id: u16, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&32u32.to_le_bytes());
    // An ordinal is a marker word followed by the value.
    out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&0xFFFFu16.to_le_bytes());
    out.extend_from_slice(&id.to_le_bytes());
    // Data version.
    out.extend_from_slice(&0u32.to_le_bytes());
    // Memory flags. Read by the sixteen bit loader and by nothing since, so
    // the value is the conventional one rather than a decision.
    out.extend_from_slice(&0x1030u16.to_le_bytes());
    // Neutral language, because an icon says nothing in any of them and a
    // stated language would make the lookup depend on the user locale.
    out.extend_from_slice(&0x0000u16.to_le_bytes());
    // Version and characteristics.
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    out.extend_from_slice(data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

fn u16le(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

fn u32le(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}