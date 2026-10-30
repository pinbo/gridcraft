//! OPC package access: reading zip parts with decompression limits, relationships and part-name
//! resolution; writing a zip package deterministically.

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};

use crate::IoError;
use crate::xml::{self, El};

/// Largest decompressed part accepted.
pub const MAX_PART: u64 = 512 * 1024 * 1024;
/// Largest total decompressed size read from one package.
pub const MAX_TOTAL: u64 = 1024 * 1024 * 1024;
/// Most entries accepted in the zip directory.
pub const MAX_ENTRIES: usize = 200_000;

pub struct Package<'a> {
    bytes: &'a [u8],
    zip: zip::ZipArchive<Cursor<&'a [u8]>>,
    /// Lower-case normalized part name → zip index.
    names: HashMap<String, usize>,
    total: u64,
}

/// An inflating part, read through [`std::io::BufRead`].
pub struct PartStream<'a> {
    reader: std::io::BufReader<CountingTake<'a>>,
    limit: u64,
    /// The limit comes from what is left of the package budget, not from the part's own maximum.
    budget_bound: bool,
}

impl<'a> PartStream<'a> {
    /// The buffered reader to parse from.
    pub fn reader(&mut self) -> &mut std::io::BufReader<CountingTake<'a>> {
        &mut self.reader
    }
}

/// A reader that stops one byte past its limit and counts what it gave out.
pub struct CountingTake<'a> {
    inner: Box<dyn Read + 'a>,
    read: u64,
    left: u64,
}

impl Read for CountingTake<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let want = buf.len().min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let Some(buf) = buf.get_mut(..want).filter(|b| !b.is_empty()) else { return Ok(0) };
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        self.left -= n as u64;
        Ok(n)
    }
}

/// A relationship.
#[derive(Clone, Debug)]
pub struct Rel {
    pub id: String,
    /// Last path segment of the relationship type (`worksheet`, `styles`, …): the same for the
    /// transitional and strict namespaces.
    pub kind: String,
    /// Resolved part name (no leading `/`), or the raw target for external relationships.
    pub target: String,
    pub external: bool,
}

pub fn normalize(name: &str) -> String {
    name.replace('\\', "/").trim_start_matches('/').to_ascii_lowercase()
}

impl<'a> Package<'a> {
    pub fn open(bytes: &'a [u8]) -> Result<Package<'a>, IoError> {
        let zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| IoError::Zip(e.to_string()))?;
        if zip.len() > MAX_ENTRIES {
            return Err(IoError::TooLarge(format!("{} zip entries", zip.len())));
        }
        let mut names = HashMap::new();
        for i in 0..zip.len() {
            if let Some(n) = zip.name_for_index(i) {
                names.entry(normalize(n)).or_insert(i);
            }
        }
        Ok(Package { bytes, zip, names, total: 0 })
    }

    pub fn has(&self, part: &str) -> bool {
        self.names.contains_key(&normalize(part))
    }

    /// Reads a part's bytes; `Ok(None)` when it doesn't exist.
    pub fn read(&mut self, part: &str) -> Result<Option<Vec<u8>>, IoError> {
        self.read_limited(part, MAX_PART)
    }

    /// Reads a small metadata part without allowing it to expand to the general part limit.
    pub fn read_limited(&mut self, part: &str, maximum: u64) -> Result<Option<Vec<u8>>, IoError> {
        let maximum = maximum.min(MAX_PART);
        let Some(&idx) = self.names.get(&normalize(part)) else {
            return Ok(None);
        };
        let mut f = self.zip.by_index(idx).map_err(|e| IoError::Zip(format!("{part}: {e}")))?;
        if f.size() > maximum {
            return Err(IoError::TooLarge(format!("{part} expands to {} bytes", f.size())));
        }
        let remaining = MAX_TOTAL.saturating_sub(self.total);
        let limit = maximum.min(remaining);
        let mut out = Vec::with_capacity(f.size().min(16 * 1024 * 1024) as usize);
        let read = (&mut f).take(limit + 1).read_to_end(&mut out).map_err(|e| {
            // A read that fails partway has already spent decompression work and buffered data
            // the package budget never sees: count the allowance as spent, so a loop over many
            // broken parts can't decompress it over and over.
            self.total = MAX_TOTAL;
            IoError::Zip(format!("{part}: {e}"))
        });
        if out.len() as u64 > limit {
            // The allowance was spent getting here; make the exhaustion terminal instead of
            // letting later reads pay the same decompression cost again.
            self.total = MAX_TOTAL;
            return Err(IoError::TooLarge(if remaining < maximum {
                "package expands to more than 1 GB".into()
            } else {
                format!("{part} expands to more than {maximum} bytes")
            }));
        }
        read?;
        self.total += out.len() as u64;
        Ok(Some(out))
    }

    /// Opens a part to be parsed while it inflates, instead of holding all of it in memory (a
    /// worksheet of millions of cells is a hundred megabytes of XML). The stream borrows the
    /// package bytes, not the package, so the parser may use it meanwhile. `Ok(None)` when the
    /// part is missing or is not stored plainly or deflated, which [`Package::read`] still handles.
    /// Call [`Package::charge`] with the finished stream: until then the budget is not spent.
    pub fn stream(&mut self, part: &str, maximum: u64) -> Result<Option<PartStream<'a>>, IoError> {
        let maximum = maximum.min(MAX_PART);
        let Some(&idx) = self.names.get(&normalize(part)) else { return Ok(None) };
        let f = self.zip.by_index_raw(idx).map_err(|e| IoError::Zip(format!("{part}: {e}")))?;
        if f.encrypted() {
            return Ok(None);
        }
        if f.size() > maximum {
            return Err(IoError::TooLarge(format!("{part} expands to {} bytes", f.size())));
        }
        let start = f.data_start() as usize;
        let Some(end) = start.checked_add(f.compressed_size() as usize) else { return Ok(None) };
        let Some(raw) = self.bytes.get(start..end) else { return Ok(None) };
        let source: Box<dyn Read + 'a> = match f.compression() {
            zip::CompressionMethod::Stored => Box::new(raw),
            zip::CompressionMethod::Deflated => Box::new(flate2::read::DeflateDecoder::new(raw)),
            _ => return Ok(None),
        };
        let remaining = MAX_TOTAL.saturating_sub(self.total);
        let limit = maximum.min(remaining);
        Ok(Some(PartStream {
            reader: std::io::BufReader::with_capacity(64 * 1024, CountingTake { inner: source, read: 0, left: limit + 1 }),
            limit,
            budget_bound: remaining < maximum,
        }))
    }

    /// Spends the package budget on a part streamed with [`Package::stream`], whether or not its
    /// parse succeeded, and refuses a part that expanded past its limit.
    pub fn charge(&mut self, part: &str, maximum: u64, stream: &PartStream<'_>) -> Result<(), IoError> {
        let read = stream.reader.get_ref().read;
        if read > stream.limit {
            // Making the exhaustion terminal is what `read_limited` does too.
            self.total = MAX_TOTAL;
            return Err(IoError::TooLarge(if stream.budget_bound {
                "package expands to more than 1 GB".into()
            } else {
                format!("{part} expands to more than {} bytes", maximum.min(MAX_PART))
            }));
        }
        self.total += read;
        Ok(())
    }

    pub fn read_xml(&mut self, part: &str) -> Result<Option<El>, IoError> {
        match self.read(part)? {
            Some(b) => xml::parse(&b).map(Some).map_err(|e| prefix(part, e)),
            None => Ok(None),
        }
    }

    /// Relationships of a part (`xl/workbook.xml` → `xl/_rels/workbook.xml.rels`). Missing or
    /// broken rels parts give an empty list.
    pub fn rels(&mut self, part: &str) -> Result<Vec<Rel>, IoError> {
        let (dir, file) = split_dir(part);
        let rels_name = if dir.is_empty() { format!("_rels/{file}.rels") } else { format!("{dir}/_rels/{file}.rels") };
        let root = match self.read(&rels_name)? {
            Some(b) => match xml::parse(&b) {
                Ok(r) => r,
                Err(_) => return Ok(vec![]),
            },
            None => return Ok(vec![]),
        };
        Ok(root
            .kids("Relationship")
            .filter_map(|r| {
                let id = r.attr("Id")?.to_string();
                let ty = r.attr("Type").unwrap_or("");
                let kind = ty.rsplit('/').next().unwrap_or("").to_string();
                let target_raw = r.attr("Target")?;
                let external = r.attr("TargetMode").is_some_and(|m| m.eq_ignore_ascii_case("External"));
                let target = if external { target_raw.to_string() } else { resolve(dir, target_raw) };
                Some(Rel { id, kind, target, external })
            })
            .collect())
    }
}

/// The first part whose declared unpacked size is over [`MAX_PART`], with that size.
pub fn oversized_part(bytes: &[u8]) -> Option<(String, u64)> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
    (0..zip.len().min(MAX_ENTRIES)).find_map(|i| {
        let f = zip.by_index_raw(i).ok()?;
        (f.size() > MAX_PART).then(|| (f.name().to_string(), f.size()))
    })
}

fn prefix(part: &str, e: IoError) -> IoError {
    match e {
        IoError::Xml(m) => IoError::Xml(format!("{part}: {m}")),
        o => o,
    }
}

/// `xl/worksheets/sheet1.xml` → (`xl/worksheets`, `sheet1.xml`).
pub fn split_dir(part: &str) -> (&str, &str) {
    let p = part.trim_start_matches('/');
    match p.rfind('/') {
        Some(i) => (p.get(..i).unwrap_or(""), p.get(i + 1..).unwrap_or("")),
        None => ("", p),
    }
}

/// Resolves a relationship target against the source part's directory.
pub fn resolve(dir: &str, target: &str) -> String {
    let target = target.replace('\\', "/");
    let target = target.split('#').next().unwrap_or("").to_string();
    let mut segs: Vec<&str> = if target.starts_with('/') { vec![] } else { dir.split('/').filter(|s| !s.is_empty()).collect() };
    for s in target.split('/') {
        match s {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            s => segs.push(s),
        }
    }
    segs.join("/")
}

// ---------------------------------------------------------------- writing

pub struct ZipOut {
    zip: zip::ZipWriter<Cursor<Vec<u8>>>,
}

impl ZipOut {
    pub fn new() -> ZipOut {
        ZipOut { zip: zip::ZipWriter::new(Cursor::new(Vec::new())) }
    }
    pub fn add(&mut self, name: &str, data: &[u8]) -> Result<(), IoError> {
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(6))
            .last_modified_time(zip::DateTime::default())
            .large_file(data.len() as u64 >= u32::MAX as u64);
        self.zip.start_file(name, opts).map_err(|e| IoError::Zip(e.to_string()))?;
        self.zip.write_all(data).map_err(|e| IoError::Zip(e.to_string()))?;
        Ok(())
    }
    pub fn finish(self) -> Result<Vec<u8>, IoError> {
        self.zip.finish().map(|c| c.into_inner()).map_err(|e| IoError::Zip(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolving() {
        assert_eq!(resolve("xl", "worksheets/sheet1.xml"), "xl/worksheets/sheet1.xml");
        assert_eq!(resolve("xl/worksheets", "../drawings/d1.xml"), "xl/drawings/d1.xml");
        assert_eq!(resolve("xl/worksheets", "/xl/tables/t.xml"), "xl/tables/t.xml");
        assert_eq!(resolve("", "xl/workbook.xml"), "xl/workbook.xml");
        assert_eq!(resolve("a", "../../../x"), "x");
        assert_eq!(split_dir("xl/workbook.xml"), ("xl", "workbook.xml"));
        assert_eq!(split_dir("x.xml"), ("", "x.xml"));
    }

    /// An over-budget read must spend the package's allowance once and for all: loops over many
    /// broken parts (a sheet's optional VML relationships, for one) otherwise re-decompress the
    /// remaining allowance again and again.
    #[test]
    fn a_failed_part_ends_the_package_budget() {
        let z = crate::tests::make_zip(&[("a.xml", b"0123456789"), ("b.xml", b"hello")]);
        let mut pkg = Package::open(&z).unwrap();
        assert!(pkg.read("a.xml").unwrap().is_some());
        // Nothing left to spend within the per-part limit either.
        pkg.total = MAX_TOTAL - 1;
        assert!(pkg.read("b.xml").is_err());
        assert_eq!(pkg.total, MAX_TOTAL, "the failed read is charged");
        // Oversized repeated attempts stay cheap errors now: no allowance is left to pay for.
        assert!(matches!(pkg.read("b.xml"), Err(IoError::TooLarge(_))));
    }

    fn zip_with(name: &str, data: &[u8], method: zip::CompressionMethod) -> Vec<u8> {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        z.start_file(name, zip::write::SimpleFileOptions::default().compression_method(method)).unwrap();
        z.write_all(data).unwrap();
        z.finish().unwrap().into_inner()
    }

    #[test]
    /// A streamed part gives the same bytes as a whole read, stored or deflated, and spends the same budget.
    fn a_streamed_part_matches_a_whole_read_and_charges_the_budget() {
        let data: Vec<u8> = (0..200_000u32).flat_map(|i| format!("<c>{i}</c>").into_bytes()).collect();
        for method in [zip::CompressionMethod::Stored, zip::CompressionMethod::Deflated] {
            let z = zip_with("x.xml", &data, method);
            let mut pkg = Package::open(&z).unwrap();
            let mut stream = pkg.stream("x.xml", MAX_PART).unwrap().expect("a stored or deflated part streams");
            let mut got = Vec::new();
            stream.reader().read_to_end(&mut got).unwrap();
            assert_eq!(got, data);
            pkg.charge("x.xml", MAX_PART, &stream).unwrap();
            assert_eq!(pkg.total, data.len() as u64);
            assert!(pkg.stream("missing.xml", MAX_PART).unwrap().is_none());
        }
    }

    #[test]
    /// A part larger than its limit is refused up front, and one that inflates past the limit is refused when charged.
    fn a_streamed_part_over_its_limit_is_refused() {
        let data = vec![b'a'; 10_000];
        let z = zip_with("big.xml", &data, zip::CompressionMethod::Deflated);
        let mut pkg = Package::open(&z).unwrap();
        assert!(matches!(pkg.stream("big.xml", 100), Err(IoError::TooLarge(_))));
        // The remaining budget is what bounds a part whose header claims less than it holds.
        pkg.total = MAX_TOTAL - 100;
        let mut stream = pkg.stream("big.xml", MAX_PART).unwrap().unwrap();
        let mut got = Vec::new();
        stream.reader().read_to_end(&mut got).unwrap();
        assert!(got.len() <= 101, "the stream stops one byte past the limit");
        assert!(matches!(pkg.charge("big.xml", MAX_PART, &stream), Err(IoError::TooLarge(_))));
        assert_eq!(pkg.total, MAX_TOTAL);
    }
}
