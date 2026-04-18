//! RSS / Atom collector.
//!
//! Parses feeds via a minimal hand-rolled XML extractor so the
//! crate stays free of an RSS-framework dependency. The feed
//! shape we care about is narrow — `<item>`/`<entry>` with
//! `title`, `link`, `description`/`summary`, `pubDate` /
//! `updated` — and that subset is robust enough to hold across
//! RSS 2.0 + Atom + Medium without bringing in `rss` +
//! `atom_syndication`.

use crate::collector::Collector;
use crate::types::Article;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub struct RssCollector {
    feeds: Vec<String>,
    http: reqwest::Client,
}

impl RssCollector {
    pub fn new(feeds: Vec<String>) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("market-maker-sentiment/0.4 (news aggregator)")
            .timeout(std::time::Duration::from_secs(15))
            .build()?;
        Ok(Self { feeds, http })
    }

    async fn fetch_one(&self, url: &str) -> Vec<Article> {
        let Ok(resp) = self.http.get(url).send().await else {
            return Vec::new();
        };
        if !resp.status().is_success() {
            return Vec::new();
        }
        let body = resp.text().await.unwrap_or_default();
        parse_feed(&body, url, Utc::now())
    }
}

#[async_trait]
impl Collector for RssCollector {
    fn name(&self) -> &'static str {
        "rss"
    }

    async fn collect(&self) -> Vec<Article> {
        let mut out = Vec::new();
        for f in &self.feeds {
            out.extend(self.fetch_one(f).await);
        }
        out
    }
}

/// Very small XML-ish extractor. We pull `<item>...</item>`
/// and `<entry>...</entry>` blocks and grep for the three
/// fields we care about inside each block. Good enough for the
/// feeds santiment targets (crypto news aggregators, mainstream
/// financial sites); upgrade to a proper parser when we start
/// tripping on escaped entities or CDATA edge cases.
pub fn parse_feed(body: &str, source_url: &str, now: DateTime<Utc>) -> Vec<Article> {
    let mut out = Vec::new();
    let blocks = split_items(body);
    for block in blocks {
        let title = extract_tag(&block, "title").unwrap_or_default();
        let link = extract_link(&block).unwrap_or_default();
        let summary = extract_tag(&block, "description")
            .or_else(|| extract_tag(&block, "summary"))
            .unwrap_or_default();
        let pub_raw = extract_tag(&block, "pubDate")
            .or_else(|| extract_tag(&block, "published"))
            .or_else(|| extract_tag(&block, "updated"));
        let published_at = pub_raw.and_then(parse_any_date);
        if link.is_empty() {
            continue;
        }
        let summary_trimmed: String = summary.chars().take(500).collect();
        out.push(Article {
            url: link,
            title,
            summary: summary_trimmed,
            source: source_url.into(),
            published_at,
            collected_at: now,
        });
    }
    // Cap per-feed output in line with santiment's 20/feed.
    out.into_iter().take(20).collect()
}

fn split_items(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = body;
    while !rest.is_empty() {
        // Look for either `<item>` (RSS) or `<entry>` (Atom).
        let (open, close) = if let Some(i) = rest.find("<item>") {
            (i, rest.find("</item>"))
        } else if let Some(i) = rest.find("<entry>") {
            (i, rest.find("</entry>"))
        } else {
            break;
        };
        let Some(end) = close else { break };
        // end is relative to rest too; take block from after the open tag
        // to just before the closing tag.
        let tag_len = if rest[open..].starts_with("<item>") { 6 } else { 7 };
        let inner_start = open + tag_len;
        if end <= inner_start {
            break;
        }
        out.push(rest[inner_start..end].to_string());
        // Advance past the closing tag.
        let consumed = end + 8.min(rest.len() - end); // `</item>` = 7, `</entry>` = 8
        if consumed >= rest.len() {
            break;
        }
        rest = &rest[consumed..];
    }
    out
}

fn extract_tag(block: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let i = block.find(&open)? + open.len();
    let j = block[i..].find(&close)? + i;
    let raw = &block[i..j];
    Some(decode_cdata_and_entities(raw))
}

fn extract_link(block: &str) -> Option<String> {
    // RSS: `<link>url</link>`. Atom: `<link href="url" .../>`.
    if let Some(v) = extract_tag(block, "link") {
        if !v.trim().is_empty() {
            return Some(v.trim().to_string());
        }
    }
    // Atom self-closing form.
    let needle = "<link";
    let i = block.find(needle)? + needle.len();
    let rest = &block[i..];
    let href_idx = rest.find("href=\"")? + 6;
    let href_rest = &rest[href_idx..];
    let end = href_rest.find('"')?;
    Some(href_rest[..end].to_string())
}

fn decode_cdata_and_entities(raw: &str) -> String {
    let mut s = raw.trim().to_string();
    if let Some(stripped) = s
        .strip_prefix("<![CDATA[")
        .and_then(|x| x.strip_suffix("]]>"))
    {
        s = stripped.to_string();
    }
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

fn parse_any_date(s: String) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(d) = DateTime::parse_from_rfc3339(s) {
        return Some(d.with_timezone(&Utc));
    }
    if let Ok(d) = DateTime::parse_from_rfc2822(s) {
        return Some(d.with_timezone(&Utc));
    }
    // Some feeds publish non-standard forms — try the most
    // common plausible patterns before giving up.
    for fmt in [
        "%a, %d %b %Y %H:%M:%S %z",
        "%a, %d %b %Y %H:%M:%S %Z",
        "%Y-%m-%dT%H:%M:%S%z",
        "%Y-%m-%d %H:%M:%S",
    ] {
        if let Ok(d) = DateTime::parse_from_str(s, fmt) {
            return Some(d.with_timezone(&Utc));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_rss_item() {
        let xml = r#"<?xml version="1.0"?>
        <rss><channel>
        <item>
          <title>Bitcoin hits new ATH</title>
          <link>https://example.com/btc-ath</link>
          <description>The flagship cryptocurrency surged to record highs.</description>
          <pubDate>Wed, 01 Apr 2026 12:00:00 +0000</pubDate>
        </item>
        </channel></rss>"#;
        let articles = parse_feed(xml, "https://example.com/feed", Utc::now());
        assert_eq!(articles.len(), 1);
        assert_eq!(articles[0].title, "Bitcoin hits new ATH");
        assert_eq!(articles[0].url, "https://example.com/btc-ath");
        assert!(articles[0].summary.starts_with("The flagship"));
        assert!(
            articles[0].published_at.is_some(),
            "pubDate should parse; raw was a valid RFC-2822 form"
        );
    }

    #[test]
    fn parse_any_date_handles_common_rss_forms() {
        // 1 April 2026 is actually a Wednesday, not a Tuesday —
        // chrono's RFC-2822 parser validates the weekday
        // against the date, so the weekday name matters.
        assert!(parse_any_date("Wed, 01 Apr 2026 12:00:00 +0000".into()).is_some());
        assert!(parse_any_date("2026-04-18T18:00:00Z".into()).is_some());
        assert!(parse_any_date("2026-04-18T18:00:00+00:00".into()).is_some());
        assert!(parse_any_date("not a date".into()).is_none());
    }

    #[test]
    fn parses_atom_entry() {
        let xml = r#"<?xml version="1.0"?>
        <feed xmlns="http://www.w3.org/2005/Atom">
        <entry>
          <title>Fed hikes rates</title>
          <link href="https://example.com/fed-hike"/>
          <summary>Decision announced at 2pm EST.</summary>
          <updated>2026-04-18T18:00:00Z</updated>
        </entry>
        </feed>"#;
        let articles = parse_feed(xml, "https://example.com/atom", Utc::now());
        assert_eq!(articles.len(), 1);
        assert_eq!(articles[0].url, "https://example.com/fed-hike");
        assert!(articles[0].published_at.is_some());
    }

    #[test]
    fn handles_cdata_sections() {
        let xml = r#"<?xml version="1.0"?>
        <rss><channel>
        <item>
          <title><![CDATA[ETH merge — big news!]]></title>
          <link>https://example.com/eth</link>
          <description><![CDATA[Includes <b>bold</b> text & entities.]]></description>
        </item>
        </channel></rss>"#;
        let articles = parse_feed(xml, "example", Utc::now());
        assert_eq!(articles.len(), 1);
        assert_eq!(articles[0].title, "ETH merge — big news!");
        assert!(articles[0].summary.contains("<b>bold</b>"));
    }

    #[test]
    fn skips_items_missing_link() {
        let xml = r#"<?xml version="1.0"?>
        <rss><channel>
        <item><title>Orphan</title></item>
        <item><title>Kept</title><link>https://example.com/k</link></item>
        </channel></rss>"#;
        let articles = parse_feed(xml, "example", Utc::now());
        assert_eq!(articles.len(), 1);
        assert_eq!(articles[0].title, "Kept");
    }
}
