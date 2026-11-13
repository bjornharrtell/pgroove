//! Decodes PostGIS geometry values, which the server sends as hex EWKB, into EWKT text
//! (`SRID=4326;POINT(1 2)`), the way `ST_AsEWKT` writes them. Done in the client so a result
//! is not wrapped in a second query.

/// The largest value (in text bytes) that is decoded; a bigger one is shown as the server
/// sent it.
pub const MAX_DECODE_BYTES: usize = 1024 * 1024;

/// Column types whose values are hex EWKB.
pub fn is_geometry_type(type_name: &str) -> bool {
    matches!(type_name, "geometry" | "geography")
}

/// Deepest nesting of collections that is followed.
const MAX_DEPTH: usize = 16;

const Z: u32 = 0x8000_0000;
const M: u32 = 0x4000_0000;
const SRID: u32 = 0x2000_0000;

/// Decodes a hex EWKB value to EWKT. `None` when the text is not a well-formed geometry,
/// so the caller can keep the original.
pub fn ewkb_hex_to_ewkt(hex: &str) -> Option<String> {
    if hex.len() > MAX_DECODE_BYTES || !hex.len().is_multiple_of(2) {
        return None;
    }
    let bytes = hex
        .as_bytes()
        .chunks(2)
        .map(|p| Some(nibble(p[0])? << 4 | nibble(p[1])?))
        .collect::<Option<Vec<u8>>>()?;
    let mut r = Reader { b: &bytes, at: 0 };
    let mut out = String::new();
    let srid = geometry(&mut r, &mut out, 0)?;
    if r.at != bytes.len() {
        return None;
    }
    Some(match srid {
        Some(s) => format!("SRID={s};{out}"),
        None => out,
    })
}

fn nibble(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let s = self.b.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }

    fn left(&self) -> usize {
        self.b.len() - self.at
    }

    fn u32(&mut self, little: bool) -> Option<u32> {
        let v: [u8; 4] = self.take(4)?.try_into().ok()?;
        Some(if little {
            u32::from_le_bytes(v)
        } else {
            u32::from_be_bytes(v)
        })
    }

    fn f64(&mut self, little: bool) -> Option<f64> {
        let v: [u8; 8] = self.take(8)?.try_into().ok()?;
        Some(if little {
            f64::from_le_bytes(v)
        } else {
            f64::from_be_bytes(v)
        })
    }
}

/// Which coordinates a geometry has beyond x and y.
#[derive(Clone, Copy)]
struct Dims {
    z: bool,
    m: bool,
}

impl Dims {
    fn count(self) -> usize {
        2 + self.z as usize + self.m as usize
    }

    /// PostGIS writes Z and ZM geometries with no tag (the extra numbers show it) and M
    /// ones as `POINTM(`.
    fn tag(self) -> &'static str {
        if self.m && !self.z { "M" } else { "" }
    }
}

/// Reads one geometry, appends its WKT, and returns its SRID when it has one.
fn geometry(r: &mut Reader, out: &mut String, depth: usize) -> Option<Option<u32>> {
    if depth > MAX_DEPTH {
        return None;
    }
    let little = match r.take(1)?[0] {
        0 => false,
        1 => true,
        _ => return None,
    };
    let raw = r.u32(little)?;
    let srid = if raw & SRID != 0 {
        Some(r.u32(little)?)
    } else {
        None
    };
    // EWKB flags, or ISO codes (1000 added for Z, 2000 for M, 3000 for both).
    let iso = (raw & 0x0FFF_FFFF) / 1000;
    let dims = Dims {
        z: raw & Z != 0 || iso == 1 || iso == 3,
        m: raw & M != 0 || iso == 2 || iso == 3,
    };
    let kind = (raw & 0x0FFF_FFFF) % 1000;
    let name = match kind {
        1 => "POINT",
        2 => "LINESTRING",
        3 => "POLYGON",
        4 => "MULTIPOINT",
        5 => "MULTILINESTRING",
        6 => "MULTIPOLYGON",
        7 => "GEOMETRYCOLLECTION",
        _ => return None,
    };
    out.push_str(name);
    out.push_str(dims.tag());
    let body_start = out.len();
    match kind {
        1 => {
            let c = coords(r, little, dims)?;
            if c.iter().all(|v| v.is_nan()) {
                out.push_str(" EMPTY");
            } else {
                out.push('(');
                push_point(out, &c);
                out.push(')');
            }
        }
        2 => points_list(r, little, dims, out)?,
        3 => {
            let rings = count(r, little, 4)?;
            list(out, rings, |out| points_list(r, little, dims, out))?;
        }
        4..=7 => {
            let n = count(r, little, 5)?;
            let multi = kind;
            list(out, n, |out| {
                // Members repeat their own header; a multipoint writes each as "x y".
                let mut inner = String::new();
                geometry(r, &mut inner, depth + 1)?;
                let wanted = match multi {
                    4 => "POINT",
                    5 => "LINESTRING",
                    6 => "POLYGON",
                    _ => "",
                };
                if !wanted.is_empty() && !inner.starts_with(wanted) {
                    return None;
                }
                if multi == 7 {
                    out.push_str(&inner);
                } else {
                    out.push_str(member_body(&inner, multi == 4));
                }
                Some(())
            })?;
        }
        _ => unreachable!(),
    }
    // An empty list writes "EMPTY" rather than "()".
    if out[body_start..].ends_with("()") {
        out.truncate(out.len() - 2);
        out.push_str(" EMPTY");
    }
    Some(srid)
}

/// The text of a member after its type name and dimension tag: "(1 2)" from "POINT(1 2)",
/// "1 2" for a point in a multipoint, and "EMPTY" for an empty one.
fn member_body(member: &str, bare_point: bool) -> &str {
    let Some(start) = member.find('(') else {
        return "EMPTY";
    };
    let body = &member[start..];
    if bare_point {
        &body[1..body.len() - 1]
    } else {
        body
    }
}

fn count(r: &mut Reader, little: bool, min_bytes_each: usize) -> Option<usize> {
    let n = r.u32(little)? as usize;
    // A count the rest of the data cannot hold is corrupt; this also bounds allocation.
    (n <= r.left() / min_bytes_each.max(1)).then_some(n)
}

fn coords(r: &mut Reader, little: bool, dims: Dims) -> Option<Vec<f64>> {
    (0..dims.count()).map(|_| r.f64(little)).collect()
}

fn push_point(out: &mut String, c: &[f64]) {
    for (i, v) in c.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&number(*v));
    }
}

fn number(v: f64) -> String {
    // `-0` and `0` read the same to people; whole numbers lose their ".0" as in PostGIS.
    if v == 0.0 {
        "0".to_string()
    } else {
        v.to_string()
    }
}

/// `(x y,x y,...)`, with `n` points read from `r`.
fn points_list(r: &mut Reader, little: bool, dims: Dims, out: &mut String) -> Option<()> {
    let n = count(r, little, dims.count() * 8)?;
    out.push('(');
    for i in 0..n {
        if i > 0 {
            out.push(',');
        }
        push_point(out, &coords(r, little, dims)?);
    }
    out.push(')');
    Some(())
}

/// Writes `n` items as `(item,item,...)`.
fn list(out: &mut String, n: usize, mut item: impl FnMut(&mut String) -> Option<()>) -> Option<()> {
    out.push('(');
    for i in 0..n {
        if i > 0 {
            out.push(',');
        }
        item(out)?;
    }
    out.push(')');
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Little-endian EWKB pieces, built here so no hex is typed by hand.
    fn header(kind: u32, srid: Option<u32>) -> Vec<u8> {
        let mut v = vec![1];
        let flags = if srid.is_some() { SRID } else { 0 };
        v.extend((kind | flags).to_le_bytes());
        if let Some(s) = srid {
            v.extend(s.to_le_bytes());
        }
        v
    }

    fn pt(c: &[f64]) -> Vec<u8> {
        c.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02X}")).collect()
    }

    fn ewkt(b: Vec<u8>) -> Option<String> {
        ewkb_hex_to_ewkt(&hex(&b))
    }

    #[test]
    fn known_point_vectors() {
        // The values PostGIS prints for ST_GeomFromText('POINT(1 2)') with and without SRID.
        assert_eq!(
            ewkb_hex_to_ewkt("0101000000000000000000F03F0000000000000040").unwrap(),
            "POINT(1 2)"
        );
        assert_eq!(
            ewkb_hex_to_ewkt("0101000020E6100000000000000000F03F0000000000000040").unwrap(),
            "SRID=4326;POINT(1 2)"
        );
    }

    #[test]
    fn big_endian_and_lowercase() {
        let mut b = vec![0u8];
        b.extend(1u32.to_be_bytes());
        b.extend(1.5f64.to_be_bytes());
        b.extend((-2.0f64).to_be_bytes());
        assert_eq!(
            ewkb_hex_to_ewkt(&hex(&b).to_lowercase()).unwrap(),
            "POINT(1.5 -2)"
        );
    }

    #[test]
    fn lines_and_polygons() {
        let mut line = header(2, None);
        line.extend(2u32.to_le_bytes());
        line.extend(pt(&[0.0, 0.0, 1.0, 1.0]));
        assert_eq!(ewkt(line).unwrap(), "LINESTRING(0 0,1 1)");

        let mut poly = header(3, Some(3857));
        poly.extend(2u32.to_le_bytes());
        for ring in [
            vec![0.0, 0.0, 4.0, 0.0, 4.0, 4.0, 0.0, 0.0],
            vec![1.0, 1.0, 2.0, 1.0, 2.0, 2.0, 1.0, 1.0],
        ] {
            poly.extend(4u32.to_le_bytes());
            poly.extend(pt(&ring));
        }
        assert_eq!(
            ewkt(poly).unwrap(),
            "SRID=3857;POLYGON((0 0,4 0,4 4,0 0),(1 1,2 1,2 2,1 1))"
        );
    }

    #[test]
    fn z_and_m_dimensions() {
        let mut p = header(1 | Z, None);
        p.extend(pt(&[1.0, 2.0, 3.0]));
        assert_eq!(ewkt(p).unwrap(), "POINT(1 2 3)");
        let mut p = header(1 | M, None);
        p.extend(pt(&[1.0, 2.0, 3.0]));
        assert_eq!(ewkt(p).unwrap(), "POINTM(1 2 3)");
        let mut p = header(1 | Z | M, None);
        p.extend(pt(&[1.0, 2.0, 3.0, 4.0]));
        assert_eq!(ewkt(p).unwrap(), "POINT(1 2 3 4)");
        // ISO codes: 1001 is a point with Z.
        let mut p = header(1001, None);
        p.extend(pt(&[1.0, 2.0, 3.0]));
        assert_eq!(ewkt(p).unwrap(), "POINT(1 2 3)");
    }

    #[test]
    fn multi_geometries_and_collections() {
        let mut mp = header(4, None);
        mp.extend(2u32.to_le_bytes());
        for c in [[0.0, 0.0], [1.0, 2.0]] {
            mp.extend(header(1, None));
            mp.extend(pt(&c));
        }
        assert_eq!(ewkt(mp).unwrap(), "MULTIPOINT(0 0,1 2)");

        let mut ml = header(5, None);
        ml.extend(1u32.to_le_bytes());
        ml.extend(header(2, None));
        ml.extend(2u32.to_le_bytes());
        ml.extend(pt(&[0.0, 0.0, 1.0, 1.0]));
        assert_eq!(ewkt(ml).unwrap(), "MULTILINESTRING((0 0,1 1))");

        let mut mpoly = header(6, None);
        mpoly.extend(1u32.to_le_bytes());
        mpoly.extend(header(3, None));
        mpoly.extend(1u32.to_le_bytes());
        mpoly.extend(4u32.to_le_bytes());
        mpoly.extend(pt(&[0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 0.0]));
        assert_eq!(ewkt(mpoly).unwrap(), "MULTIPOLYGON(((0 0,1 0,1 1,0 0)))");

        let mut gc = header(7, Some(4326));
        gc.extend(2u32.to_le_bytes());
        gc.extend(header(1, None));
        gc.extend(pt(&[5.0, 6.0]));
        gc.extend(header(2, None));
        gc.extend(2u32.to_le_bytes());
        gc.extend(pt(&[0.0, 0.0, 1.0, 1.0]));
        assert_eq!(
            ewkt(gc).unwrap(),
            "SRID=4326;GEOMETRYCOLLECTION(POINT(5 6),LINESTRING(0 0,1 1))"
        );
    }

    #[test]
    fn empty_geometries() {
        let mut p = header(1, None);
        p.extend(pt(&[f64::NAN, f64::NAN]));
        assert_eq!(ewkt(p).unwrap(), "POINT EMPTY");
        let mut l = header(2, None);
        l.extend(0u32.to_le_bytes());
        assert_eq!(ewkt(l).unwrap(), "LINESTRING EMPTY");
        let mut g = header(7, None);
        g.extend(0u32.to_le_bytes());
        assert_eq!(ewkt(g).unwrap(), "GEOMETRYCOLLECTION EMPTY");
        let mut poly = header(3, None);
        poly.extend(0u32.to_le_bytes());
        assert_eq!(ewkt(poly).unwrap(), "POLYGON EMPTY");
    }

    #[test]
    fn malformed_input_is_left_alone() {
        assert_eq!(ewkb_hex_to_ewkt(""), None);
        assert_eq!(ewkb_hex_to_ewkt("zz"), None);
        assert_eq!(ewkb_hex_to_ewkt("010"), None, "odd length");
        assert_eq!(ewkb_hex_to_ewkt("0201000000"), None, "bad byte order");
        assert_eq!(ewkb_hex_to_ewkt("0163000000"), None, "unknown type");
        // Cut short, and with bytes left over.
        assert_eq!(ewkb_hex_to_ewkt("01010000000000"), None);
        let mut p = header(1, None);
        p.extend(pt(&[1.0, 2.0]));
        p.push(0);
        assert_eq!(ewkt(p), None);
        // A count far larger than the data cannot allocate or loop.
        let mut l = header(2, None);
        l.extend(u32::MAX.to_le_bytes());
        assert_eq!(ewkt(l), None);
        // A member of the wrong kind.
        let mut mp = header(4, None);
        mp.extend(1u32.to_le_bytes());
        mp.extend(header(2, None));
        mp.extend(0u32.to_le_bytes());
        assert_eq!(ewkt(mp), None);
        // Collections nested too deep.
        let mut deep = Vec::new();
        for _ in 0..40 {
            deep.extend(header(7, None));
            deep.extend(1u32.to_le_bytes());
        }
        assert_eq!(ewkt(deep), None);
        // Over the size cap.
        assert_eq!(ewkb_hex_to_ewkt(&"0".repeat(MAX_DECODE_BYTES + 2)), None);
    }

    #[test]
    fn which_types_are_geometry() {
        assert!(is_geometry_type("geometry"));
        assert!(is_geometry_type("geography"));
        assert!(!is_geometry_type("text"));
        assert!(!is_geometry_type("bytea"));
    }

    #[test]
    fn numbers_read_like_postgis() {
        assert_eq!(number(0.0), "0");
        assert_eq!(number(-0.0), "0");
        assert_eq!(number(10.0), "10");
        assert_eq!(number(0.1), "0.1");
        assert_eq!(number(-122.4194), "-122.4194");
    }
}
