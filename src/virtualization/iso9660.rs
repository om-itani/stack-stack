//! A minimal ISO9660 (+Joliet) image writer, used to hand a cloud-init NoCloud
//! seed to a guest without requiring `genisoimage`, `mkisofs`, `xorriso` or
//! `cloud-localds` on the host.
//!
//! Only the subset ECMA-119 needs for a seed volume is produced: a primary
//! descriptor, a Joliet supplementary descriptor, both path tables (one record,
//! the root), the file extents, and the two root directories. Joliet carries the
//! real file names (`user-data`, `meta-data`, …), because plain ISO9660 level 1
//! cannot represent a hyphen; every Linux `isofs` mount picks the Joliet tree
//! automatically.
//!
//! cloud-init finds the volume by its label, so the volume id is `CIDATA`. The
//! layout is byte-for-byte deterministic, which is what makes it testable.

/// Every volume descriptor and every directory record is measured in logical
/// blocks of this size.
const SECTOR: usize = 2048;

/// Reserved system area, 16 sectors.
#[cfg_attr(not(test), allow(dead_code))]
const SYSTEM_AREA_SECTORS: usize = 16;

const PVD_SECTOR: usize = 16;
const SVD_SECTOR: usize = 17;
const TERMINATOR_SECTOR: usize = 18;
const LPATH_SECTOR: usize = 19;
const MPATH_SECTOR: usize = 20;

/// A single-record path table: `len_di | ext_attr | extent | parent | name`,
/// `1 + 1 + 4 + 2 + 1 = 9`, padded to an even 10 bytes.
const PATH_TABLE_SIZE: u32 = 10;

/// Fixed timestamps (2024-01-01T00:00:00+00) keep the image reproducible.
const D_DATE7: [u8; 7] = [124, 1, 1, 0, 0, 0, 0];
const D_DATE16: &[u8; 16] = b"2024010100000000";

/// One file placed at the root of the seed volume.
pub struct IsoFile<'a> {
    /// Bare name, e.g. `user-data`; encoded to UCS-2 for the Joliet tree.
    pub name: &'a str,
    pub data: &'a [u8],
}

struct Placed {
    extent: u32,
    length: u32,
}

/// Build a NoCloud seed image with the given volume id (`CIDATA` in practice).
pub fn build(volume_id: &str, files: &[IsoFile<'_>]) -> Vec<u8> {
    let volume_id = sanitize_volume_id(volume_id);

    // 1. Assign each file a sector-aligned extent after the path tables.
    let mut placed: Vec<Placed> = Vec::with_capacity(files.len());
    let mut cursor = (MPATH_SECTOR + 1) as u32;
    for file in files {
        let length = file.data.len() as u32;
        placed.push(Placed {
            extent: cursor,
            length,
        });
        // An empty file consumes no sector, so several can share one extent.
        cursor += length.div_ceil(SECTOR as u32);
    }

    // 2. The root directories. Their size depends only on the file names, so
    //    every LBA (and therefore every count) is known before anything is
    //    written.
    let primary_root_lba = cursor;
    let primary_root = root_directory(&[], primary_root_lba);
    cursor += (primary_root.len() / SECTOR) as u32;

    let joliet_root_lba = cursor;
    let children = file_records(files, &placed);
    let joliet_root = root_directory(&children, joliet_root_lba);
    cursor += (joliet_root.len() / SECTOR) as u32;

    let total_sectors = cursor as usize;
    let mut image = vec![0u8; total_sectors * SECTOR];

    // 3. Descriptors, path tables and extents.
    write_descriptor(
        &mut image[PVD_SECTOR * SECTOR..],
        DescriptorKind::Primary,
        &volume_id,
        total_sectors as u32,
        primary_root_lba,
        primary_root.len() as u32,
    );
    write_descriptor(
        &mut image[SVD_SECTOR * SECTOR..],
        DescriptorKind::Joliet,
        &volume_id,
        total_sectors as u32,
        joliet_root_lba,
        joliet_root.len() as u32,
    );
    write_terminator(&mut image[TERMINATOR_SECTOR * SECTOR..]);
    write_path_table(&mut image[LPATH_SECTOR * SECTOR..], primary_root_lba, false);
    write_path_table(&mut image[MPATH_SECTOR * SECTOR..], primary_root_lba, true);

    for (file, spot) in files.iter().zip(&placed) {
        let start = spot.extent as usize * SECTOR;
        image[start..start + file.data.len()].copy_from_slice(file.data);
    }

    image[primary_root_lba as usize * SECTOR..][..primary_root.len()]
        .copy_from_slice(&primary_root);
    image[joliet_root_lba as usize * SECTOR..][..joliet_root.len()].copy_from_slice(&joliet_root);

    image
}

#[derive(Clone, Copy)]
enum DescriptorKind {
    Primary,
    Joliet,
}

/// The two mandatory `.`/`..` entries plus any children, sector padded.
///
/// Both special identifiers are single bytes in either encoding: `0x00` for the
/// directory itself and `0x01` for its parent (itself, at the root).
fn root_directory(children: &[Vec<u8>], root_lba: u32) -> Vec<u8> {
    // Build once with a placeholder length to learn the padded total, then
    // rebuild the two special records with the real directory length.
    let mut probe = vec![
        dir_record(&[0x00], root_lba, SECTOR as u32, true),
        dir_record(&[0x01], root_lba, SECTOR as u32, true),
    ];
    probe.extend_from_slice(children);
    let dir_len = pack_directory(&probe).len() as u32;

    let mut records = vec![
        dir_record(&[0x00], root_lba, dir_len, true),
        dir_record(&[0x01], root_lba, dir_len, true),
    ];
    records.extend_from_slice(children);
    pack_directory(&records)
}

/// Joliet directory records for every file, pointing at the shared extents.
fn file_records(files: &[IsoFile<'_>], placed: &[Placed]) -> Vec<Vec<u8>> {
    let mut records = Vec::with_capacity(files.len());
    for (file, spot) in files.iter().zip(placed) {
        let name = file
            .name
            .encode_utf16()
            .flat_map(u16::to_be_bytes)
            .collect::<Vec<u8>>();
        records.push(dir_record(&name, spot.extent, spot.length, false));
    }
    records
}

fn dir_record(name: &[u8], extent: u32, data_length: u32, is_dir: bool) -> Vec<u8> {
    let raw = 33 + name.len();
    let len = if raw.is_multiple_of(2) { raw } else { raw + 1 };
    let mut record = vec![0u8; len];
    record[0] = len as u8;
    record[1] = 0;
    put_both_u32(&mut record, 2, extent);
    put_both_u32(&mut record, 10, data_length);
    record[18..25].copy_from_slice(&D_DATE7);
    record[25] = if is_dir { 0x02 } else { 0x00 };
    record[26] = 0;
    record[27] = 0;
    put_both_u16(&mut record, 28, 1);
    record[32] = name.len() as u8;
    record[33..33 + name.len()].copy_from_slice(name);
    record
}

/// Concatenate directory records, starting a fresh sector whenever the next
/// record would cross a boundary (records must not span sectors), then pad the
/// result up to a whole number of sectors.
fn pack_directory(records: &[Vec<u8>]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for record in records {
        let offset_in_sector = out.len() % SECTOR;
        if offset_in_sector + record.len() > SECTOR {
            out.resize(out.len() + (SECTOR - offset_in_sector), 0);
        }
        out.extend_from_slice(record);
    }
    let remainder = out.len() % SECTOR;
    if remainder != 0 {
        out.resize(out.len() + (SECTOR - remainder), 0);
    }
    out
}

fn write_descriptor(
    sector: &mut [u8],
    kind: DescriptorKind,
    volume_id: &str,
    total_sectors: u32,
    root_lba: u32,
    root_len: u32,
) {
    let joliet = matches!(kind, DescriptorKind::Joliet);
    sector[0] = if joliet { 2 } else { 1 };
    sector[1..6].copy_from_slice(b"CD001");
    sector[6] = 1;

    if joliet {
        // Escape sequences declare the Joliet level.
        sector[88..91].copy_from_slice(b"%/@");
        sector[91..94].copy_from_slice(b"%/C");
        sector[94..97].copy_from_slice(b"%/E");
    }

    write_str_field(&mut sector[8..40], "stack-stack", joliet);
    write_str_field(&mut sector[40..72], volume_id, joliet);
    put_both_u32(sector, 80, total_sectors);
    put_both_u16(sector, 120, 1);
    put_both_u16(sector, 124, 1);
    put_both_u16(sector, 128, SECTOR as u16);
    put_both_u32(sector, 132, PATH_TABLE_SIZE);
    put_le_u32(sector, 140, LPATH_SECTOR as u32);
    put_le_u32(sector, 144, 0);
    put_be_u32(sector, 148, MPATH_SECTOR as u32);
    put_be_u32(sector, 152, 0);

    // Root directory record: 33 bytes plus a single-byte identifier.
    let root = dir_record(&[0x00], root_lba, root_len, true);
    sector[156..156 + root.len()].copy_from_slice(&root);

    // Text identifiers are left blank (spaces / UCS-2 spaces).
    for offset in [190usize, 318, 446, 574] {
        write_str_field(&mut sector[offset..offset + 128], "", joliet);
    }
    for offset in [702usize, 739, 776] {
        write_str_field(&mut sector[offset..offset + 37], "", joliet);
    }

    if joliet {
        for offset in [813usize, 830, 847, 864] {
            write_dec_date(&mut sector[offset..offset + 17]);
        }
        sector[881] = 1; // file structure version
    } else {
        for offset in [941usize, 958, 975, 992] {
            write_dec_date(&mut sector[offset..offset + 17]);
        }
        sector[1009] = 1; // file structure version
    }
}

/// One-volume descriptor set terminator.
fn write_terminator(sector: &mut [u8]) {
    sector[0] = 255;
    sector[1..6].copy_from_slice(b"CD001");
    sector[6] = 1;
}

fn write_path_table(sector: &mut [u8], root_lba: u32, big_endian: bool) {
    sector[0] = 1; // length of the root directory identifier
    sector[1] = 0; // extended attribute length
    if big_endian {
        sector[2..6].copy_from_slice(&root_lba.to_be_bytes());
        sector[6..8].copy_from_slice(&1u16.to_be_bytes());
    } else {
        sector[2..6].copy_from_slice(&root_lba.to_le_bytes());
        sector[6..8].copy_from_slice(&1u16.to_le_bytes());
    }
    sector[8] = 0x00; // root identifier
    sector[9] = 0; // padding to an even length
}

/// ASCII (primary) or UCS-2BE (Joliet) string field, space padded.
fn write_str_field(field: &mut [u8], value: &str, joliet: bool) {
    if joliet {
        let mut encoded: Vec<u8> = value.encode_utf16().flat_map(u16::to_be_bytes).collect();
        encoded.truncate(field.len());
        // Pad with UCS-2 spaces, but never overshoot an odd-length field: the
        // final byte is filled with a NUL when only one byte remains.
        while encoded.len() + 2 <= field.len() {
            encoded.extend_from_slice(&[0x00, 0x20]);
        }
        while encoded.len() < field.len() {
            encoded.push(0x00);
        }
        field.copy_from_slice(&encoded);
    } else {
        let bytes = value.as_bytes();
        for (index, slot) in field.iter_mut().enumerate() {
            *slot = *bytes.get(index).unwrap_or(&b' ');
        }
    }
}

fn write_dec_date(field: &mut [u8]) {
    for (index, slot) in field.iter_mut().enumerate() {
        *slot = *D_DATE16.get(index).unwrap_or(&0x00);
    }
}

fn put_both_u32(out: &mut [u8], offset: usize, value: u32) {
    out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    out[offset + 4..offset + 8].copy_from_slice(&value.to_be_bytes());
}

fn put_both_u16(out: &mut [u8], offset: usize, value: u16) {
    out[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    out[offset + 2..offset + 4].copy_from_slice(&value.to_be_bytes());
}

fn put_le_u32(out: &mut [u8], offset: usize, value: u32) {
    out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_be_u32(out: &mut [u8], offset: usize, value: u32) {
    out[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

/// ISO9660 d-characters are `A-Z0-9_`; anything else is dropped and the result
/// is upper-cased and truncated to a legal volume identifier.
pub fn sanitize_volume_id(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(32)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if cleaned.is_empty() {
        "CIDATA".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> Vec<u8> {
        build(
            "cidata",
            &[
                IsoFile {
                    name: "meta-data",
                    data: b"instance-id: iid-1\n",
                },
                IsoFile {
                    name: "user-data",
                    data: b"#cloud-config\nhostname: web\n",
                },
                IsoFile {
                    name: "network-config",
                    data: b"version: 2\n",
                },
            ],
        )
    }

    fn ucs2(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_be_bytes).collect()
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        !needle.is_empty()
            && haystack
                .windows(needle.len())
                .any(|window| window == needle)
    }

    #[test]
    fn image_is_sector_aligned_and_starts_with_the_system_area() {
        let image = seed();
        assert_eq!(image.len() % SECTOR, 0);
        assert!(image.len() > SYSTEM_AREA_SECTORS * SECTOR);
        assert!(
            image[..SYSTEM_AREA_SECTORS * SECTOR]
                .iter()
                .all(|b| *b == 0)
        );
    }

    #[test]
    fn descriptors_are_well_formed() {
        let image = seed();
        let pvd = &image[PVD_SECTOR * SECTOR..];
        assert_eq!(pvd[0], 1, "primary descriptor type");
        assert_eq!(&pvd[1..6], b"CD001");
        assert_eq!(pvd[6], 1, "descriptor version");

        let svd = &image[SVD_SECTOR * SECTOR..];
        assert_eq!(svd[0], 2, "supplementary descriptor type");
        assert_eq!(&svd[88..97], b"%/@%/C%/E", "Joliet escape sequences");

        let term = &image[TERMINATOR_SECTOR * SECTOR..];
        assert_eq!(term[0], 255);
        assert_eq!(&term[1..6], b"CD001");
    }

    #[test]
    fn volume_label_is_the_cloud_init_marker() {
        let image = seed();
        let pvd = &image[PVD_SECTOR * SECTOR..];
        let label = std::str::from_utf8(&pvd[40..72]).expect("ascii");
        assert_eq!(label.trim_end(), "CIDATA");
    }

    #[test]
    fn both_endian_volume_size_agrees_and_matches_the_file() {
        let image = seed();
        let pvd = &image[PVD_SECTOR * SECTOR..];
        let le = u32::from_le_bytes(pvd[80..84].try_into().expect("le"));
        let be = u32::from_be_bytes(pvd[84..88].try_into().expect("be"));
        assert_eq!(le, be);
        assert_eq!(le as usize, image.len() / SECTOR);
    }

    #[test]
    fn joliet_tree_carries_the_exact_cloud_init_filenames() {
        let image = seed();
        assert!(contains(&image, &ucs2("user-data")));
        assert!(contains(&image, &ucs2("meta-data")));
        assert!(contains(&image, &ucs2("network-config")));
    }

    #[test]
    fn every_file_body_is_present_verbatim() {
        let image = seed();
        assert!(contains(&image, b"#cloud-config\nhostname: web\n"));
        assert!(contains(&image, b"instance-id: iid-1\n"));
    }

    #[test]
    fn root_directory_records_point_at_their_own_extent() {
        let image = seed();
        let pvd = &image[PVD_SECTOR * SECTOR..];
        let root_lba = u32::from_le_bytes(pvd[156 + 2..156 + 6].try_into().expect("lba"));
        let root = &image[root_lba as usize * SECTOR..];
        // The first record is ".", length 34, flags directory (0x02).
        assert_eq!(root[0], 34);
        assert_eq!(root[25] & 0x02, 0x02);
        let self_lba = u32::from_le_bytes(root[2..6].try_into().expect("lba"));
        assert_eq!(self_lba, root_lba);
    }

    #[test]
    fn volume_ids_are_sanitized() {
        assert_eq!(sanitize_volume_id("cidata"), "CIDATA");
        assert_eq!(sanitize_volume_id("my seed!"), "MYSEED");
        assert_eq!(sanitize_volume_id(""), "CIDATA");
    }
}
