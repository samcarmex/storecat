//! The one search index over every volume (Tantivy).
//!
//! Each file, directory and archive entry is one document. Names are indexed twice: as
//! words (exact and prefix matches, cheap and high-signal) and as 3-grams with positions
//! (so "nvoic" finds "ACME_Invoice.pdf"). Paths, extracted text and metadata words are
//! indexed as words. Everything the result list needs is stored on the document, so a
//! search never opens a per-volume database.
//!
//! Schema v2. When the on-disk schema differs the directory is recreated and the caller
//! rebuilds from the per-volume databases, which remain the source of truth.

pub mod query;

use std::ops::Bound;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

use tantivy::collector::{Count, TopDocs};
use tantivy::query::{
    AllQuery, BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, PhraseQuery, Query, RangeQuery,
    TermQuery,
};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, FAST, INDEXED, STORED,
    STRING,
};
use tantivy::tokenizer::{
    LowerCaser, SimpleTokenizer, TextAnalyzer, Token, TokenStream, Tokenizer,
};
use tantivy::{Index, IndexReader, IndexWriter, Order, ReloadPolicy, TantivyDocument, Term};

use crate::error::{Error, Result};
use crate::model::{flags, ItemType, Kind, SortBy};
use crate::scan::{is_placeholder, DirBatch};
use query::ParsedQuery;

const WRITER_HEAP_BYTES: usize = 256 * 1024 * 1024;
const NGRAM: usize = 3;
/// A name's first ten characters packed six bits each, so sorting the number sorts names
/// A to Z: spaces and punctuation first, then digits, then letters (case ignored), then
/// anything else. Names that share their first ten characters tie.
pub fn name_key(name: &str) -> u64 {
    let mut key = 0u64;
    let mut n = 0;
    for c in name.chars().flat_map(char::to_lowercase) {
        if n == 10 {
            break;
        }
        let code = match c {
            '0'..='9' => 2 + (c as u64 - '0' as u64),
            'a'..='z' => 12 + (c as u64 - 'a' as u64),
            c if c.is_alphabetic() => 38,
            _ => 1,
        };
        key = (key << 6) | code;
        n += 1;
    }
    key << (6 * (10 - n))
}

/// Bumped when documents change shape without the Tantivy schema changing (a new flag
/// bit, say): the index is then rebuilt from the catalogues on the next start.
pub const SCHEMA_VERSION: u32 = 5;
const VERSION_FILE: &str = "storecat-index-version";

#[derive(Clone, Copy)]
pub struct Fields {
    pub doc_key: Field,
    pub parent_key: Field,
    pub volume_id: Field,
    pub item_id: Field,
    pub item_type: Field,
    pub name: Field,
    pub name_ngram: Field,
    pub path: Field,
    pub ext: Field,
    pub kind: Field,
    pub size: Field,
    pub mtime: Field,
    pub flags: Field,
    pub flag: Field,
    pub content: Field,
    pub meta: Field,
    /// Tag words, tokenised (free-text matches).
    pub tags: Field,
    /// Whole tags, lowercased, for `tag:` filters.
    pub tag: Field,
    /// The name's first characters packed into a number, for sorting by name.
    pub name_key: Field,
}

pub struct SearchIndex {
    #[allow(dead_code)] // keeps the directory and tokenizer registry alive
    index: Index,
    reader: IndexReader,
    writer: Mutex<IndexWriter<TantivyDocument>>,
    pub fields: Fields,
}

/// Everything needed to build one document.
pub struct DocInput<'a> {
    pub volume_id: i64,
    pub item_id: i64,
    pub item_type: ItemType,
    pub name: &'a str,
    pub path: &'a str,
    pub ext: &'a str,
    pub kind: Kind,
    pub size: u64,
    pub mtime: i64,
    pub flags: u64,
    /// For archive entries: the archive file's doc key. 0 otherwise.
    pub parent_key: u64,
    pub content: Option<&'a str>,
    pub meta_text: Option<&'a str>,
    pub tags: &'a [String],
}

/// A raw hit before the catalogue joins volume information onto it.
#[derive(Debug, Clone)]
pub struct RawHit {
    pub volume_id: u64,
    pub item_id: u64,
    pub item_type: ItemType,
    pub name: String,
    pub path: String,
    pub ext: String,
    pub kind: Kind,
    pub size: u64,
    pub mtime: i64,
    pub flags: u64,
    pub score: f32,
}

pub struct RawResults {
    pub hits: Vec<RawHit>,
    pub total: usize,
    pub elapsed_ms: f64,
}

/// Unique key of a document: volume (22 bits), type (2 bits), item id (40 bits).
pub fn doc_key(volume_id: i64, item_type: ItemType, item_id: i64) -> u64 {
    ((volume_id as u64) << 42)
        | ((item_type as u64) << 40)
        | ((item_id as u64) & ((1u64 << 40) - 1))
}

fn build_schema() -> (Schema, Fields) {
    let mut sb = Schema::builder();
    let words_stored = TextOptions::default()
        .set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("words")
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        )
        .set_stored();
    let words = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("words")
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let ngram = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer("ngram3")
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let fields = Fields {
        doc_key: sb.add_u64_field("doc_key", INDEXED | FAST),
        parent_key: sb.add_u64_field("parent_key", INDEXED),
        volume_id: sb.add_u64_field("volume_id", INDEXED | STORED | FAST),
        item_id: sb.add_u64_field("item_id", STORED | FAST),
        item_type: sb.add_u64_field("item_type", INDEXED | STORED | FAST),
        name: sb.add_text_field("name", words_stored.clone()),
        name_ngram: sb.add_text_field("name_ngram", ngram),
        path: sb.add_text_field("path", words_stored),
        ext: sb.add_text_field("ext", STRING | STORED | FAST),
        kind: sb.add_u64_field("kind", INDEXED | STORED | FAST),
        size: sb.add_u64_field("size", INDEXED | STORED | FAST),
        mtime: sb.add_i64_field("mtime", INDEXED | STORED | FAST),
        flags: sb.add_u64_field("flags", STORED | FAST),
        flag: sb.add_u64_field("flag", INDEXED),
        content: sb.add_text_field("content", words.clone()),
        meta: sb.add_text_field("meta", words.clone()),
        tags: sb.add_text_field("tags", words),
        tag: sb.add_text_field("tag", STRING),
        name_key: sb.add_u64_field("name_key", FAST),
    };
    (sb.build(), fields)
}

fn register_tokenizers(index: &Index) {
    index.tokenizers().register(
        "words",
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(LowerCaser)
            .build(),
    );
    index.tokenizers().register(
        "ngram3",
        TextAnalyzer::builder(PositionedNgram { n: NGRAM })
            .filter(LowerCaser)
            .build(),
    );
}

/// Character n-grams with *incrementing positions*, so a `PhraseQuery` over the grams of
/// a query string matches exactly that substring. Tantivy's own `NgramTokenizer` gives
/// every gram position 0, which makes phrase matching impossible.
#[derive(Clone)]
struct PositionedNgram {
    n: usize,
}

struct PositionedNgramStream<'a> {
    text: &'a str,
    bounds: Vec<usize>,
    n: usize,
    pos: usize,
    token: Token,
}

impl Tokenizer for PositionedNgram {
    type TokenStream<'a> = PositionedNgramStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        let mut bounds: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        bounds.push(text.len());
        PositionedNgramStream {
            text,
            bounds,
            n: self.n,
            pos: 0,
            token: Token::default(),
        }
    }
}

impl TokenStream for PositionedNgramStream<'_> {
    fn advance(&mut self) -> bool {
        if self.pos + self.n + 1 > self.bounds.len() {
            return false;
        }
        let from = self.bounds[self.pos];
        let to = self.bounds[self.pos + self.n];
        self.token.position = self.pos;
        self.token.offset_from = from;
        self.token.offset_to = to;
        self.token.text.clear();
        self.token.text.push_str(&self.text[from..to]);
        self.pos += 1;
        true
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.token
    }
}

impl SearchIndex {
    /// Open the index, or create it. The boolean is true when an incompatible index was
    /// discarded (or none existed) and the caller should rebuild from the volume databases.
    pub fn open_or_create(dir: &Path) -> Result<(Self, bool)> {
        std::fs::create_dir_all(dir)?;
        let (schema, fields) = build_schema();
        let mut fresh = false;
        let mmap = tantivy::directory::MmapDirectory::open(dir)?;
        let mut index = if Index::exists(&mmap)? {
            Some(Index::open(mmap)?)
        } else {
            None
        };
        if let Some(existing) = &index {
            let version = std::fs::read_to_string(dir.join(VERSION_FILE))
                .ok()
                .and_then(|v| v.trim().parse::<u32>().ok());
            if existing.schema() != schema || version != Some(SCHEMA_VERSION) {
                log::warn!("search index format changed; rebuilding");
                index = None;
            }
        }
        let index = match index {
            Some(i) => i,
            None => {
                fresh = true;
                // Wipe whatever was there so Tantivy starts clean.
                if dir.exists() {
                    for entry in std::fs::read_dir(dir)?.flatten() {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
                let mmap = tantivy::directory::MmapDirectory::open(dir)?;
                let created = Index::create(mmap, schema, tantivy::IndexSettings::default())?;
                std::fs::write(dir.join(VERSION_FILE), SCHEMA_VERSION.to_string())?;
                created
            }
        };
        register_tokenizers(&index);
        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .clamp(1, 8);
        // A previous Storecat that is still shutting down (an update, a quick relaunch) holds
        // the writer lock for a few seconds: wait for it rather than fail to start.
        let mut waited = 0;
        let writer = loop {
            match index.writer_with_num_threads(threads, WRITER_HEAP_BYTES) {
                Ok(w) => break w,
                Err(tantivy::TantivyError::LockFailure(_, _)) if waited < 150 => {
                    if waited == 0 {
                        log::warn!("search index is locked by another Storecat; waiting");
                    }
                    waited += 1;
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => return Err(e.into()),
            }
        };
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok((
            Self {
                index,
                reader,
                writer: Mutex::new(writer),
                fields,
            },
            fresh,
        ))
    }

    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    fn make_doc(&self, d: &DocInput) -> TantivyDocument {
        let f = self.fields;
        let mut doc = TantivyDocument::new();
        doc.add_u64(f.doc_key, doc_key(d.volume_id, d.item_type, d.item_id));
        doc.add_u64(f.parent_key, d.parent_key);
        doc.add_u64(f.volume_id, d.volume_id as u64);
        doc.add_u64(f.item_id, d.item_id as u64);
        doc.add_u64(f.item_type, d.item_type as u64);
        doc.add_text(f.name, d.name);
        doc.add_text(f.name_ngram, d.name);
        doc.add_u64(f.name_key, name_key(d.name));
        doc.add_text(f.path, d.path);
        doc.add_text(f.ext, d.ext);
        doc.add_u64(f.kind, d.kind as u8 as u64);
        doc.add_u64(f.size, d.size);
        doc.add_i64(f.mtime, d.mtime);
        let flags = if d.item_type == ItemType::File && crate::places::is_os_leftover(d.name) {
            d.flags | flags::OS_LEFTOVER
        } else {
            d.flags
        };
        doc.add_u64(f.flags, flags);
        for bit in 0..8 {
            if flags & (1 << bit) != 0 {
                doc.add_u64(f.flag, 1 << bit);
            }
        }
        if let Some(c) = d.content {
            doc.add_text(f.content, c);
        }
        if let Some(m) = d.meta_text {
            doc.add_text(f.meta, m);
        }
        for t in d.tags {
            doc.add_text(f.tags, t);
            doc.add_text(f.tag, t.to_lowercase());
        }
        doc
    }

    /// Add one document. Safe to call from many threads at once.
    pub fn add_doc(&self, d: &DocInput) -> Result<()> {
        let doc = self.make_doc(d);
        let writer = self.writer.lock().map_err(|_| poisoned())?;
        writer.add_document(doc)?;
        Ok(())
    }

    /// Replace a document (and, for files, any archive-entry documents under it).
    pub fn replace_doc(&self, d: &DocInput) -> Result<()> {
        let key = doc_key(d.volume_id, d.item_type, d.item_id);
        let doc = self.make_doc(d);
        let writer = self.writer.lock().map_err(|_| poisoned())?;
        writer.delete_term(Term::from_field_u64(self.fields.doc_key, key));
        writer.add_document(doc)?;
        Ok(())
    }

    /// Remove the archive-entry documents that hang off a file.
    pub fn delete_children(&self, parent_key: u64) -> Result<()> {
        let writer = self.writer.lock().map_err(|_| poisoned())?;
        writer.delete_term(Term::from_field_u64(self.fields.parent_key, parent_key));
        Ok(())
    }

    /// Index a directory and its files straight from the scanner.
    pub fn add_batch(&self, volume_id: i64, batch: &DirBatch) -> Result<()> {
        let parent_path = parent_of(&batch.rel_path);
        let mut docs = Vec::with_capacity(batch.files.len() + 1);
        if batch.dir_id != 0 {
            docs.push(self.make_doc(&DocInput {
                volume_id,
                item_id: batch.dir_id,
                item_type: ItemType::Dir,
                name: &batch.name,
                path: parent_path,
                ext: "",
                kind: Kind::Dir,
                size: 0,
                mtime: batch.mtime,
                flags: 0,
                parent_key: 0,
                content: None,
                meta_text: None,
                tags: &[],
            }));
        }
        for file in &batch.files {
            docs.push(self.make_doc(&DocInput {
                volume_id,
                item_id: file.id,
                item_type: ItemType::File,
                name: &file.name,
                path: &batch.rel_path,
                ext: &file.ext,
                kind: file.kind,
                size: file.size,
                mtime: file.mtime,
                flags: if is_placeholder(file.attrs) {
                    flags::PLACEHOLDER
                } else {
                    0
                },
                parent_key: 0,
                content: None,
                meta_text: None,
                tags: &[],
            }));
        }
        let writer = self.writer.lock().map_err(|_| poisoned())?;
        for doc in docs {
            writer.add_document(doc)?;
        }
        Ok(())
    }

    pub fn delete_volume(&self, volume_id: i64) -> Result<()> {
        let writer = self.writer.lock().map_err(|_| poisoned())?;
        writer.delete_term(Term::from_field_u64(
            self.fields.volume_id,
            volume_id as u64,
        ));
        Ok(())
    }

    pub fn commit(&self) -> Result<()> {
        {
            let mut writer = self.writer.lock().map_err(|_| poisoned())?;
            writer.commit()?;
        }
        self.reader.reload()?;
        Ok(())
    }

    pub fn rollback(&self) -> Result<()> {
        let mut writer = self.writer.lock().map_err(|_| poisoned())?;
        writer.rollback()?;
        Ok(())
    }

    /// Run a parsed query. `volume_filter` restricts to those volume ids (None = all).
    pub fn search(
        &self,
        parsed: &ParsedQuery,
        volume_filter: Option<&[i64]>,
        sort: SortBy,
        limit: usize,
        offset: usize,
    ) -> Result<RawResults> {
        let started = Instant::now();
        let query = self.build_query(parsed, volume_filter);
        let searcher = self.reader.searcher();
        let f = self.fields;

        let top = || TopDocs::with_limit(limit.max(1)).and_offset(offset);
        let by = |field: &str, order: Order| -> Result<(Vec<(f32, tantivy::DocAddress)>, usize)> {
            let (hits, count) = match field {
                "mtime" => {
                    let (t, c) = searcher.search(
                        &query,
                        &(top().order_by_fast_field::<i64>("mtime", order), Count),
                    )?;
                    (t.into_iter().map(|(_, a)| a).collect::<Vec<_>>(), c)
                }
                f => {
                    let (t, c) = searcher
                        .search(&query, &(top().order_by_fast_field::<u64>(f, order), Count))?;
                    (t.into_iter().map(|(_, a)| a).collect(), c)
                }
            };
            Ok((hits.into_iter().map(|a| (0.0, a)).collect(), count))
        };
        let (addrs, total): (Vec<(f32, tantivy::DocAddress)>, usize) = match sort {
            SortBy::Relevance => searcher.search(&query, &(top(), Count))?,
            SortBy::ModifiedDesc => by("mtime", Order::Desc)?,
            SortBy::ModifiedAsc => by("mtime", Order::Asc)?,
            SortBy::SizeDesc => by("size", Order::Desc)?,
            SortBy::SizeAsc => by("size", Order::Asc)?,
            SortBy::NameAsc => by("name_key", Order::Asc)?,
            SortBy::NameDesc => by("name_key", Order::Desc)?,
        };

        let mut hits = Vec::with_capacity(addrs.len());
        for (score, addr) in addrs {
            let d: TantivyDocument = searcher.doc(addr)?;
            let get_u64 = |field: Field| d.get_first(field).and_then(|v| v.as_u64()).unwrap_or(0);
            let get_i64 = |field: Field| d.get_first(field).and_then(|v| v.as_i64()).unwrap_or(0);
            let get_str = |field: Field| {
                d.get_first(field)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            hits.push(RawHit {
                volume_id: get_u64(f.volume_id),
                item_id: get_u64(f.item_id),
                item_type: ItemType::from_u8(get_u64(f.item_type) as u8),
                name: get_str(f.name),
                path: get_str(f.path),
                ext: get_str(f.ext),
                kind: Kind::from_u8(get_u64(f.kind) as u8),
                size: get_u64(f.size),
                mtime: get_i64(f.mtime),
                flags: get_u64(f.flags),
                score,
            });
        }
        Ok(RawResults {
            hits,
            total,
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        })
    }

    fn build_query(&self, p: &ParsedQuery, volume_filter: Option<&[i64]>) -> Box<dyn Query> {
        let f = self.fields;
        let mut must: Vec<(Occur, Box<dyn Query>)> = Vec::new();

        for term in &p.terms {
            must.push((Occur::Must, self.term_query(term)));
        }
        for phrase in &p.phrases {
            must.push((Occur::Must, self.phrase_query(phrase)));
        }
        for term in &p.path_terms {
            let t = Term::from_field_text(f.path, term);
            let q: Box<dyn Query> = Box::new(BooleanQuery::new(vec![
                (
                    Occur::Should,
                    Box::new(TermQuery::new(t.clone(), IndexRecordOption::Basic)),
                ),
                (
                    Occur::Should,
                    Box::new(FuzzyTermQuery::new_prefix(t, 0, true)),
                ),
            ]));
            must.push((Occur::Must, q));
        }
        for term in &p.content_terms {
            let t = Term::from_field_text(f.content, term);
            must.push((
                Occur::Must,
                Box::new(TermQuery::new(t, IndexRecordOption::WithFreqs)),
            ));
        }
        for phrase in &p.content_phrases {
            let terms: Vec<Term> = phrase
                .split_whitespace()
                .map(|w| Term::from_field_text(f.content, w))
                .collect();
            let q: Box<dyn Query> = if terms.len() == 1 {
                Box::new(TermQuery::new(
                    terms.into_iter().next().unwrap(),
                    IndexRecordOption::WithFreqs,
                ))
            } else {
                Box::new(PhraseQuery::new(terms))
            };
            must.push((Occur::Must, q));
        }
        if !p.exts.is_empty() {
            must.push((
                Occur::Must,
                any_of(p.exts.iter().map(|e| Term::from_field_text(f.ext, e))),
            ));
        }
        if !p.kinds.is_empty() {
            must.push((
                Occur::Must,
                any_of(
                    p.kinds
                        .iter()
                        .map(|k| Term::from_field_u64(f.kind, *k as u8 as u64)),
                ),
            ));
        }
        if let Some(t) = p.item_type {
            must.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_u64(f.item_type, t as u64),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if !p.tags.is_empty() {
            for t in &p.tags {
                must.push((
                    Occur::Must,
                    Box::new(TermQuery::new(
                        Term::from_field_text(f.tag, t),
                        IndexRecordOption::Basic,
                    )),
                ));
            }
        }
        for bit in &p.has_flags {
            must.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_u64(f.flag, *bit),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if p.size_min.is_some() || p.size_max.is_some() {
            let lower = p.size_min.map(Bound::Included).unwrap_or(Bound::Unbounded);
            let upper = p.size_max.map(Bound::Included).unwrap_or(Bound::Unbounded);
            must.push((
                Occur::Must,
                Box::new(RangeQuery::new_u64_bounds("size".to_string(), lower, upper)),
            ));
        }
        if p.mtime_min.is_some() || p.mtime_max.is_some() {
            let lower = p.mtime_min.map(Bound::Included).unwrap_or(Bound::Unbounded);
            let upper = p.mtime_max.map(Bound::Excluded).unwrap_or(Bound::Unbounded);
            must.push((
                Occur::Must,
                Box::new(RangeQuery::new_i64_bounds(
                    "mtime".to_string(),
                    lower,
                    upper,
                )),
            ));
        }
        if let Some(ids) = volume_filter {
            must.push((
                Occur::Must,
                any_of(
                    ids.iter()
                        .map(|id| Term::from_field_u64(f.volume_id, *id as u64)),
                ),
            ));
        }

        let leftover = || -> Box<dyn Query> {
            Box::new(TermQuery::new(
                Term::from_field_u64(f.flag, flags::OS_LEFTOVER),
                IndexRecordOption::Basic,
            ))
        };
        if p.leftovers {
            must.push((Occur::Must, leftover()));
        } else {
            if must.is_empty() {
                must.push((Occur::Must, Box::new(AllQuery)));
            }
            must.push((Occur::MustNot, leftover()));
        }
        let base: Box<dyn Query> = Box::new(BooleanQuery::new(must));
        let demote = p.item_type.is_none() && (!p.terms.is_empty() || !p.phrases.is_empty());
        if !demote {
            return base;
        }
        // Folders are useful hits but a short folder name wins BM25's length norm against
        // any file with the same word in it; archive entries are usually noise next to the
        // real file. Every document matches exactly one branch, so these are multipliers.
        let typed = |t: ItemType| -> Box<dyn Query> {
            Box::new(TermQuery::new(
                Term::from_field_u64(f.item_type, t as u64),
                IndexRecordOption::Basic,
            ))
        };
        let branch = |t: ItemType, boost: f32| -> (Occur, Box<dyn Query>) {
            let q = BooleanQuery::new(vec![
                (Occur::Must, base.box_clone()),
                (Occur::Must, typed(t)),
            ]);
            (Occur::Should, Box::new(BoostQuery::new(Box::new(q), boost)))
        };
        Box::new(BooleanQuery::new(vec![
            branch(ItemType::File, 1.0),
            branch(ItemType::ArchiveEntry, 0.7),
            branch(ItemType::Dir, 0.5),
        ]))
    }

    /// One free-text term: exact word in name (best), word prefix in name, substring via
    /// n-grams, then metadata words, path words and document text at lower weights.
    fn term_query(&self, term: &str) -> Box<dyn Query> {
        let f = self.fields;
        let mut shoulds: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        let boosted = |q: Box<dyn Query>, b: f32| -> (Occur, Box<dyn Query>) {
            (Occur::Should, Box::new(BoostQuery::new(q, b)))
        };
        let name_term = Term::from_field_text(f.name, term);
        shoulds.push(boosted(
            Box::new(TermQuery::new(
                name_term.clone(),
                IndexRecordOption::WithFreqs,
            )),
            4.0,
        ));
        shoulds.push(boosted(
            Box::new(FuzzyTermQuery::new_prefix(name_term, 0, true)),
            2.5,
        ));
        if let Some(q) = self.ngram_phrase(term) {
            shoulds.push(boosted(q, 1.5));
        }
        let tag_term = Term::from_field_text(f.tags, term);
        shoulds.push(boosted(
            Box::new(TermQuery::new(
                tag_term.clone(),
                IndexRecordOption::WithFreqs,
            )),
            3.0,
        ));
        shoulds.push(boosted(
            Box::new(FuzzyTermQuery::new_prefix(tag_term, 0, true)),
            2.0,
        ));
        let meta_term = Term::from_field_text(f.meta, term);
        shoulds.push(boosted(
            Box::new(TermQuery::new(
                meta_term.clone(),
                IndexRecordOption::WithFreqs,
            )),
            1.4,
        ));
        shoulds.push(boosted(
            Box::new(FuzzyTermQuery::new_prefix(meta_term, 0, true)),
            0.9,
        ));
        let path_term = Term::from_field_text(f.path, term);
        shoulds.push(boosted(
            Box::new(TermQuery::new(
                path_term.clone(),
                IndexRecordOption::WithFreqs,
            )),
            0.8,
        ));
        shoulds.push(boosted(
            Box::new(FuzzyTermQuery::new_prefix(path_term, 0, true)),
            0.5,
        ));
        let content_term = Term::from_field_text(f.content, term);
        shoulds.push(boosted(
            Box::new(TermQuery::new(content_term, IndexRecordOption::WithFreqs)),
            0.6,
        ));
        Box::new(BooleanQuery::new(shoulds))
    }

    fn ngram_phrase(&self, term: &str) -> Option<Box<dyn Query>> {
        let chars: Vec<char> = term.chars().collect();
        if chars.len() < NGRAM {
            return None;
        }
        let grams: Vec<Term> = chars
            .windows(NGRAM)
            .map(|w| Term::from_field_text(self.fields.name_ngram, &w.iter().collect::<String>()))
            .collect();
        if grams.len() == 1 {
            return Some(Box::new(TermQuery::new(
                grams.into_iter().next().unwrap(),
                IndexRecordOption::Basic,
            )));
        }
        Some(Box::new(PhraseQuery::new(grams)))
    }

    /// A quoted phrase: the words in order, in the name, the path or the text.
    fn phrase_query(&self, phrase: &str) -> Box<dyn Query> {
        let f = self.fields;
        let words: Vec<&str> = phrase.split_whitespace().collect();
        if words.len() == 1 {
            return self.term_query(words[0]);
        }
        let mk = |field: Field| -> Box<dyn Query> {
            Box::new(PhraseQuery::new(
                words
                    .iter()
                    .map(|w| Term::from_field_text(field, w))
                    .collect(),
            ))
        };
        Box::new(BooleanQuery::new(vec![
            (Occur::Should, Box::new(BoostQuery::new(mk(f.name), 2.0))),
            (Occur::Should, mk(f.path)),
            (Occur::Should, mk(f.meta)),
            (Occur::Should, Box::new(BoostQuery::new(mk(f.content), 0.8))),
        ]))
    }
}

fn any_of(terms: impl Iterator<Item = Term>) -> Box<dyn Query> {
    let shoulds: Vec<(Occur, Box<dyn Query>)> = terms
        .map(|t| {
            (
                Occur::Should,
                Box::new(TermQuery::new(t, IndexRecordOption::Basic)) as Box<dyn Query>,
            )
        })
        .collect();
    Box::new(BooleanQuery::new(shoulds))
}

pub fn parent_of(rel_path: &str) -> &str {
    match rel_path.rfind('/') {
        Some(i) => &rel_path[..i],
        None => "",
    }
}

fn poisoned() -> Error {
    Error::Other("index writer lock poisoned".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Kind;
    use crate::scan::FileEntry;

    fn file(id: i64, name: &str, size: u64, mtime: i64) -> FileEntry {
        let ext = crate::kind::split_ext(name);
        FileEntry {
            id,
            kind: crate::kind::kind_for_ext(&ext),
            name: name.into(),
            ext,
            size,
            mtime,
            ctime: mtime,
            attrs: 0,
        }
    }

    fn sample_index() -> (tempfile::TempDir, SearchIndex) {
        let tmp = tempfile::tempdir().unwrap();
        let (idx, fresh) = SearchIndex::open_or_create(tmp.path()).unwrap();
        assert!(fresh);
        idx.add_batch(
            7,
            &DirBatch {
                dir_id: 0,
                parent_id: None,
                name: String::new(),
                rel_path: String::new(),
                mtime: 0,
                files: vec![file(1, "readme.txt", 10, 1_600_000_000)],
            },
        )
        .unwrap();
        idx.add_batch(
            7,
            &DirBatch {
                dir_id: 3,
                parent_id: Some(0),
                name: "ACME".into(),
                rel_path: "Documents/Clients/ACME".into(),
                mtime: 0,
                files: vec![
                    file(2, "ACME_Invoice_2023-04.pdf", 1_200_000, 1_681_000_000),
                    file(3, "acme-invoice-march.docx", 86_000, 1_677_700_000),
                    file(4, "IMG_4471.jpg", 6_200_000, 1_675_300_000),
                ],
            },
        )
        .unwrap();
        idx.add_batch(
            9,
            &DirBatch {
                dir_id: 0,
                parent_id: None,
                name: String::new(),
                rel_path: String::new(),
                mtime: 0,
                files: vec![file(
                    1,
                    "ACME shoot day 2 A001_C004.mov",
                    18_400_000_000,
                    1_714_700_000,
                )],
            },
        )
        .unwrap();
        idx.commit().unwrap();
        (tmp, idx)
    }

    fn names(r: &RawResults) -> Vec<String> {
        r.hits.iter().map(|h| h.name.clone()).collect()
    }

    #[test]
    fn finds_by_word_prefix_and_substring() {
        let (_t, idx) = sample_index();
        let q = query::parse("invoice");
        let r = idx.search(&q, None, SortBy::Relevance, 10, 0).unwrap();
        assert_eq!(r.total, 2);

        let q = query::parse("inv");
        let r = idx.search(&q, None, SortBy::Relevance, 10, 0).unwrap();
        assert_eq!(r.total, 2, "prefix should match");

        let q = query::parse("nvoic");
        let r = idx.search(&q, None, SortBy::Relevance, 10, 0).unwrap();
        assert_eq!(r.total, 2, "substring via ngrams should match");

        let q = query::parse("acme");
        let r = idx.search(&q, None, SortBy::Relevance, 10, 0).unwrap();
        assert_eq!(r.total, 5);
        assert_eq!(
            r.hits[0].item_type,
            ItemType::File,
            "a file should rank first, got {:?}",
            r.hits[0].name
        );
        assert_eq!(r.hits.last().unwrap().name, "IMG_4471.jpg");
    }

    #[test]
    fn filters_apply() {
        let (_t, idx) = sample_index();
        let r = idx
            .search(
                &query::parse("acme ext:pdf"),
                None,
                SortBy::Relevance,
                10,
                0,
            )
            .unwrap();
        assert_eq!(names(&r), vec!["ACME_Invoice_2023-04.pdf"]);
        let r = idx
            .search(&query::parse("kind:video"), None, SortBy::Relevance, 10, 0)
            .unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.hits[0].kind, Kind::Video);
        let r = idx
            .search(&query::parse("size:>1gb"), None, SortBy::Relevance, 10, 0)
            .unwrap();
        assert_eq!(r.total, 1);
        let r = idx
            .search(
                &query::parse("modified:2023"),
                None,
                SortBy::Relevance,
                10,
                0,
            )
            .unwrap();
        assert_eq!(r.total, 3);
        let r = idx
            .search(&query::parse("acme"), Some(&[9]), SortBy::Relevance, 10, 0)
            .unwrap();
        assert_eq!(r.total, 1);
        let r = idx
            .search(&query::parse("is:dir"), None, SortBy::Relevance, 10, 0)
            .unwrap();
        assert_eq!(names(&r), vec!["ACME"]);
    }

    #[test]
    fn content_and_meta_and_entries() {
        let (_t, idx) = sample_index();
        // Enrich the invoice: text, meta, a thumbnail flag.
        idx.replace_doc(&DocInput {
            volume_id: 7,
            item_id: 2,
            item_type: ItemType::File,
            name: "ACME_Invoice_2023-04.pdf",
            path: "Documents/Clients/ACME",
            ext: "pdf",
            kind: Kind::Document,
            size: 1_200_000,
            mtime: 1_681_000_000,
            flags: flags::HAS_TEXT | flags::HAS_META | flags::HAS_THUMB,
            parent_key: 0,
            content: Some("Total due AUD 4,180.00. Unpaid balance carried from March."),
            meta_text: Some("Sam Carmex Acrobat"),
            tags: &["print-ready".to_string(), "acme".to_string()],
        })
        .unwrap();
        // An archive with one entry.
        let zip_key = doc_key(7, ItemType::File, 5);
        idx.add_doc(&DocInput {
            volume_id: 7,
            item_id: 5,
            item_type: ItemType::File,
            name: "backup.zip",
            path: "",
            ext: "zip",
            kind: Kind::Archive,
            size: 10,
            mtime: 0,
            flags: 0,
            parent_key: 0,
            content: None,
            meta_text: None,
            tags: &[],
        })
        .unwrap();
        idx.add_doc(&DocInput {
            volume_id: 7,
            item_id: 100,
            item_type: ItemType::ArchiveEntry,
            name: "secret_recipe.txt",
            path: "backup.zip/notes",
            ext: "txt",
            kind: Kind::Document,
            size: 3,
            mtime: 0,
            flags: 0,
            parent_key: zip_key,
            content: None,
            meta_text: None,
            tags: &[],
        })
        .unwrap();
        idx.commit().unwrap();

        let r = idx
            .search(&query::parse("unpaid"), None, SortBy::Relevance, 10, 0)
            .unwrap();
        assert_eq!(
            names(&r),
            vec!["ACME_Invoice_2023-04.pdf"],
            "plain terms search text"
        );
        let r = idx
            .search(
                &query::parse("content:balance"),
                None,
                SortBy::Relevance,
                10,
                0,
            )
            .unwrap();
        assert_eq!(r.total, 1);
        let r = idx
            .search(
                &query::parse("content:\"unpaid balance\""),
                None,
                SortBy::Relevance,
                10,
                0,
            )
            .unwrap();
        assert_eq!(r.total, 1);
        let r = idx
            .search(&query::parse("carmex"), None, SortBy::Relevance, 10, 0)
            .unwrap();
        assert_eq!(r.total, 1, "meta words are searchable");
        let r = idx
            .search(
                &query::parse("tag:print-ready"),
                None,
                SortBy::Relevance,
                10,
                0,
            )
            .unwrap();
        assert_eq!(r.total, 1, "whole-tag filter");
        let r = idx
            .search(&query::parse("ready"), None, SortBy::Relevance, 10, 0)
            .unwrap();
        assert_eq!(r.total, 1, "tag words are free-text searchable");
        let r = idx
            .search(&query::parse("has:thumb"), None, SortBy::Relevance, 10, 0)
            .unwrap();
        assert_eq!(r.total, 1);
        assert!(r.hits[0].flags & flags::HAS_THUMB != 0);
        let r = idx
            .search(&query::parse("recipe"), None, SortBy::Relevance, 10, 0)
            .unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.hits[0].item_type, ItemType::ArchiveEntry);
        // Total count unchanged for the replaced doc (no duplicate).
        let r = idx
            .search(&query::parse("invoice"), None, SortBy::Relevance, 10, 0)
            .unwrap();
        assert_eq!(r.total, 2);
        // Dropping entries by parent.
        idx.delete_children(zip_key).unwrap();
        idx.commit().unwrap();
        let r = idx
            .search(&query::parse("recipe"), None, SortBy::Relevance, 10, 0)
            .unwrap();
        assert_eq!(r.total, 0);
    }

    #[test]
    fn sorts_and_deletes() {
        let (_t, idx) = sample_index();
        let r = idx
            .search(&query::parse(""), None, SortBy::SizeDesc, 10, 0)
            .unwrap();
        assert_eq!(r.hits[0].name, "ACME shoot day 2 A001_C004.mov");
        let r = idx
            .search(&query::parse(""), None, SortBy::ModifiedDesc, 2, 0)
            .unwrap();
        assert_eq!(r.hits.len(), 2);
        assert!(r.hits[0].mtime >= r.hits[1].mtime);
        idx.delete_volume(7).unwrap();
        idx.commit().unwrap();
        let r = idx
            .search(&query::parse(""), None, SortBy::Relevance, 10, 0)
            .unwrap();
        assert_eq!(r.total, 1);
    }

    #[test]
    fn name_key_sorts_names() {
        let mut names = vec![
            "zebra.mp3",
            "Alpha.mp3",
            "_notes",
            "10 songs",
            "apple",
            "Äpfel",
            "2 songs",
        ];
        names.sort_by_key(|n| name_key(n));
        assert_eq!(
            names,
            vec![
                "_notes",
                "10 songs",
                "2 songs",
                "Alpha.mp3",
                "apple",
                "zebra.mp3",
                "Äpfel"
            ]
        );
        assert!(name_key("abc") < name_key("abcd"));
    }
}
