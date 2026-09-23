//! SPIR-V blobs produced by build.rs.

pub static UI_VERT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ui.vert.spv"));
pub static UI_FRAG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ui.frag.spv"));

/// Converts a byte blob into the word slice vkCreateShaderModule expects.
/// The magic number tells the byte order; SPIR-V produced on the same host
/// is always little endian, the swap path exists for completeness.
pub fn words(bytes: &[u8]) -> Vec<u32> {
    const MAGIC: u32 = 0x0723_0203;
    assert!(bytes.len() % 4 == 0, "SPIR-V blob is not word aligned");
    assert!(bytes.len() >= 20, "SPIR-V blob is too short");

    let mut out = Vec::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        out.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    if out[0] == MAGIC.swap_bytes() {
        for w in out.iter_mut() {
            *w = w.swap_bytes();
        }
    }
    assert_eq!(out[0], MAGIC, "not a SPIR-V module");
    out
}