use crate::content::{Bytes, Chunk, IdentRef};
use crate::error::Result;
use crate::ids::{ByteRange, EntityId};
use crate::lang::Resolution;

#[derive(Clone, Copy)]
enum Hole {
    Child(EntityId),
    Name(EntityId),
}

pub fn bytes_from_span(
    src: &[u8],
    extent: ByteRange,
    children: &[(ByteRange, EntityId)],
    own_name: Option<(ByteRange, EntityId)>,
    resolution: &Resolution,
) -> Result<Bytes> {
    let mut holes: Vec<(ByteRange, Hole)> = children
        .iter()
        .map(|(r, id)| (*r, Hole::Child(*id)))
        .collect();
    if let Some((r, id)) = own_name {
        holes.push((r, Hole::Name(id)));
    }
    for (r, ident) in &resolution.refs {
        if let IdentRef::Entity(id) = ident {
            if *id != EntityId::SELF {
                holes.push((*r, Hole::Name(*id)));
            }
        }
    }
    holes.sort_by_key(|(r, _)| r.start);
    holes.retain(|(r, _)| r.start >= extent.start && r.end <= extent.end && r.end > r.start);

    let mut out_src = Vec::new();
    let mut chunks = Vec::new();
    let mut pos = extent.start;
    for (r, hole) in &holes {
        if r.start < pos {
            continue;
        }
        if r.start > pos {
            emit_lit(src, pos, r.start, &mut out_src, &mut chunks);
        }
        match hole {
            Hole::Child(id) => chunks.push(Chunk::Child(*id)),
            Hole::Name(id) => chunks.push(Chunk::Name(*id)),
        }
        pos = r.end;
    }
    if pos < extent.end {
        emit_lit(src, pos, extent.end, &mut out_src, &mut chunks);
    }

    let local_ranges = remap_locals(&out_src, src, extent, &holes, resolution);
    Bytes::new(out_src, chunks, local_ranges)
}

fn emit_lit(src: &[u8], start: u32, end: u32, out: &mut Vec<u8>, chunks: &mut Vec<Chunk>) {
    let a = start as usize;
    let b = end as usize;
    if a >= b || b > src.len() {
        return;
    }
    let lo = out.len() as u32;
    out.extend_from_slice(&src[a..b]);
    let hi = out.len() as u32;
    if hi > lo {
        chunks.push(Chunk::Literal(ByteRange { start: lo, end: hi }));
    }
}

fn remap_locals(
    out_src: &[u8],
    file: &[u8],
    extent: ByteRange,
    holes: &[(ByteRange, Hole)],
    resolution: &Resolution,
) -> Vec<(ByteRange, IdentRef)> {
    let _ = (out_src, file);
    let mut out = Vec::new();
    for (r, ident) in &resolution.refs {
        if !matches!(ident, IdentRef::Local(_, _) | IdentRef::Free(_)) {
            continue;
        }
        if r.start < extent.start || r.end > extent.end {
            continue;
        }
        if holes.iter().any(|(h, _)| r.start >= h.start && r.end <= h.end) {
            continue;
        }
        let skipped: u32 = holes
            .iter()
            .filter(|(h, _)| h.end <= r.start)
            .map(|(h, _)| h.len())
            .sum();
        let start = r.start - extent.start - skipped;
        let end = r.end - extent.start - skipped;
        out.push((ByteRange { start, end }, ident.clone()));
    }
    for (r, _slot, _ns) in &resolution.slots {
        // binders that are Local in ident_ranges: already via refs if we record them.
        let _ = r;
    }
    out
}
