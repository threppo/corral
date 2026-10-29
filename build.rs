//! Embed the application manifest and icon without external tools: write a COFF object whose
//! `.rsrc` section holds RT_ICON #1..n and RT_GROUP_ICON #1 (split from `assets/icon/corral.ico`,
//! which `scripts/build-icon.ps1` packs) plus RT_MANIFEST #1 (the same layout `windres` emits), and
//! link it into the binary. Group icon #1 is the exe's only icon group, so Explorer, the taskbar and
//! Alt-Tab show it for corral.exe; windows can load it with `LoadIconW(hinstance, 1)`.
//! The manifest declares Windows 8+ (required for layered child windows), PerMonitorV2 DPI
//! awareness and Common Controls v6.

use std::{env, fs, path::PathBuf};

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0" xmlns:asmv3="urn:schemas-microsoft-com:asm.v3">
  <assemblyIdentity type="win32" name="Corral" version="0.1.0.0" processorArchitecture="amd64"/>
  <dependency><dependentAssembly>
    <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
  </dependentAssembly></dependency>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1"><application>
    <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    <supportedOS Id="{1f676c76-80e1-4239-95bb-83d0f6d0da78}"/>
    <supportedOS Id="{4a2f28e3-53b9-4441-ba9c-d69d4a4a6e38}"/>
  </application></compatibility>
  <asmv3:application><asmv3:windowsSettings>
    <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
  </asmv3:windowsSettings></asmv3:application>
</assembly>
"#;

const ICON: &str = "assets/icon/corral.ico";
const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;
const RT_MANIFEST: u32 = 24;
const LANG_EN_US: u32 = 0x409;

struct Resource {
    kind: u32,
    id: u32,
    data: Vec<u8>,
}

fn u16le(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn u32le(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Split an .ico file into its images (RT_ICON #1..n) and the RT_GROUP_ICON directory naming them:
/// each 16-byte ICONDIRENTRY becomes a 14-byte GRPICONDIRENTRY whose file offset is replaced by the
/// image's resource ID.
fn icon_resources(ico: &[u8]) -> Vec<Resource> {
    let u16_at = |i: usize| u16::from_le_bytes([ico[i], ico[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([ico[i], ico[i + 1], ico[i + 2], ico[i + 3]]);
    assert!(ico.len() >= 6 && u16_at(0) == 0 && u16_at(2) == 1, "{ICON} is not an icon file");
    let count = u16_at(4) as usize;
    assert!(count > 0 && ico.len() >= 6 + 16 * count, "{ICON} has a truncated directory");
    let mut group = Vec::new();
    u16le(&mut group, 0); // reserved
    u16le(&mut group, 1); // type: icon
    u16le(&mut group, count as u16);
    let mut resources = Vec::new();
    for n in 0..count {
        let entry = 6 + 16 * n;
        let (size, offset) = (u32_at(entry + 8) as usize, u32_at(entry + 12) as usize);
        assert!(offset + size <= ico.len(), "{ICON} image {n} lies outside the file");
        let id = n as u32 + 1;
        group.extend_from_slice(&ico[entry..entry + 12]); // size, colours, planes, bit count, byte count
        u16le(&mut group, id as u16);
        resources.push(Resource { kind: RT_ICON, id, data: ico[offset..offset + size].to_vec() });
    }
    resources.push(Resource { kind: RT_GROUP_ICON, id: 1, data: group });
    resources
}

/// Resource directory table with ID entries only.
fn directory(out: &mut Vec<u8>, entries: &[(u32, u32)]) {
    out.extend_from_slice(&[0u8; 12]); // characteristics, timestamp, major/minor version
    u16le(out, 0); // named entries
    u16le(out, entries.len() as u16); // id entries
    for &(id, target) in entries {
        u32le(out, id);
        u32le(out, target);
    }
}

/// `resources` must be sorted by (kind, id): every directory level lists its IDs in ascending order.
/// Layout: root (one entry per kind), one ID directory per kind, one language directory and one
/// data entry per resource, then the data. Each data entry's RVA gets an ADDR32NB relocation.
fn resource_object(resources: &[Resource]) -> Vec<u8> {
    const SUBDIR: u32 = 0x8000_0000;
    let dir_size = |entries: usize| (16 + 8 * entries) as u32;
    let mut kinds: Vec<u32> = resources.iter().map(|r| r.kind).collect();
    kinds.dedup();
    let mut at = dir_size(kinds.len());
    let mut kind_dirs = Vec::new();
    for kind in &kinds {
        kind_dirs.push(at);
        at += dir_size(resources.iter().filter(|r| r.kind == *kind).count());
    }
    let lang_dirs = at;
    at += dir_size(1) * resources.len() as u32;
    let data_entries = at;
    at += 16 * resources.len() as u32;
    let mut data_at = Vec::new();
    for r in resources {
        data_at.push(at);
        at += (r.data.len() as u32 + 7) & !7;
    }

    let mut rsrc = Vec::new();
    directory(&mut rsrc, &kinds.iter().zip(&kind_dirs).map(|(&k, &d)| (k, SUBDIR | d)).collect::<Vec<_>>());
    for kind in &kinds {
        let ids: Vec<(u32, u32)> = (0..resources.len())
            .filter(|&i| resources[i].kind == *kind)
            .map(|i| (resources[i].id, SUBDIR | (lang_dirs + dir_size(1) * i as u32)))
            .collect();
        directory(&mut rsrc, &ids);
    }
    for i in 0..resources.len() {
        directory(&mut rsrc, &[(LANG_EN_US, data_entries + 16 * i as u32)]);
    }
    for (r, &offset) in resources.iter().zip(&data_at) {
        u32le(&mut rsrc, offset); // data RVA (relocated, ADDR32NB)
        u32le(&mut rsrc, r.data.len() as u32);
        u32le(&mut rsrc, 0); // code page
        u32le(&mut rsrc, 0); // reserved
    }
    for r in resources {
        rsrc.extend_from_slice(&r.data);
        while rsrc.len() % 8 != 0 {
            rsrc.push(0);
        }
    }
    assert_eq!(rsrc.len() as u32, at);

    let relocations = resources.len() as u32;
    let raw_offset = 20 + 40;
    let reloc_offset = raw_offset + rsrc.len() as u32;
    let symtab_offset = reloc_offset + 10 * relocations;
    let mut obj = Vec::new();
    u16le(&mut obj, 0x8664); // AMD64
    u16le(&mut obj, 1); // sections
    u32le(&mut obj, 0); // timestamp
    u32le(&mut obj, symtab_offset);
    u32le(&mut obj, 1); // symbols
    u16le(&mut obj, 0); // optional header size
    u16le(&mut obj, 0); // characteristics
    obj.extend_from_slice(b".rsrc\0\0\0");
    u32le(&mut obj, 0); // virtual size
    u32le(&mut obj, 0); // virtual address
    u32le(&mut obj, rsrc.len() as u32);
    u32le(&mut obj, raw_offset);
    u32le(&mut obj, reloc_offset);
    u32le(&mut obj, 0); // line numbers
    u16le(&mut obj, relocations as u16);
    u16le(&mut obj, 0);
    u32le(&mut obj, 0x4000_0040); // INITIALIZED_DATA | MEM_READ
    obj.extend_from_slice(&rsrc);
    for i in 0..relocations {
        u32le(&mut obj, data_entries + 16 * i); // the data entry's RVA field
        u32le(&mut obj, 0); // symbol index (.rsrc)
        u16le(&mut obj, 3); // IMAGE_REL_AMD64_ADDR32NB
    }
    obj.extend_from_slice(b".rsrc\0\0\0");
    u32le(&mut obj, 0); // value
    u16le(&mut obj, 1); // section number
    u16le(&mut obj, 0); // type
    obj.push(3); // IMAGE_SYM_CLASS_STATIC
    obj.push(0); // aux symbols
    u32le(&mut obj, 4); // empty string table
    obj
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={ICON}");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let ico_path = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join(ICON);
    let ico = fs::read(&ico_path).unwrap_or_else(|e| panic!("{}: {e} (run scripts/build-icon.ps1)", ico_path.display()));
    let mut resources = icon_resources(&ico);
    resources.push(Resource { kind: RT_MANIFEST, id: 1, data: MANIFEST.as_bytes().to_vec() });
    resources.sort_by_key(|r| (r.kind, r.id));
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("corral-resources.o");
    fs::write(&out, resource_object(&resources)).unwrap();
    println!("cargo:rustc-link-arg-bins={}", out.display());
}
