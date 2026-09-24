//! Revision-specific exports from the saved artifact, never a stale chat copy.
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::io::{Cursor, Write};
fn xml(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t' | '\r'))
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn docx(source: &str) -> Result<Vec<u8>> {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
    let mut body = String::new();
    let mut paragraph = false;
    let mut bold = false;
    let mut italic = false;
    let mut cell = false;
    let mut head = false;
    let mut strike = false;
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut links: Vec<String> = Vec::new();
    fn close(body: &mut String, p: &mut bool) {
        if *p {
            body.push_str("</w:p>");
            *p = false;
        }
    }
    for event in Parser::new_ext(
        source,
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH,
    ) {
        match event {
            Event::Start(Tag::Paragraph) => {
                if !paragraph {
                    body.push_str("<w:p>");
                    paragraph = true;
                }
            }
            Event::Start(Tag::Heading { level, .. }) => {
                close(&mut body, &mut paragraph);
                body.push_str(&format!(
                    "<w:p><w:pPr><w:pStyle w:val=\"Heading{}\"/></w:pPr>",
                    level as u8
                ));
                paragraph = true;
            }
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_)) => close(&mut body, &mut paragraph),
            Event::Start(Tag::Strong) => bold = true,
            Event::End(TagEnd::Strong) => bold = false,
            Event::Start(Tag::Emphasis) => italic = true,
            Event::End(TagEnd::Emphasis) => italic = false,
            Event::Start(Tag::Table(_)) => {
                close(&mut body, &mut paragraph);
                body.push_str("<w:tbl><w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/><w:tblBorders><w:top w:val=\"single\" w:sz=\"4\"/><w:left w:val=\"single\" w:sz=\"4\"/><w:bottom w:val=\"single\" w:sz=\"4\"/><w:right w:val=\"single\" w:sz=\"4\"/><w:insideH w:val=\"single\" w:sz=\"4\"/><w:insideV w:val=\"single\" w:sz=\"4\"/></w:tblBorders></w:tblPr>");
            }
            Event::End(TagEnd::Table) => body.push_str("</w:tbl>"),
            Event::Start(Tag::TableHead) => {
                head = true;
                body.push_str("<w:tr>");
            }
            Event::End(TagEnd::TableHead) => {
                head = false;
                body.push_str("</w:tr>");
            }
            Event::Start(Tag::TableRow) => body.push_str("<w:tr>"),
            Event::End(TagEnd::TableRow) => body.push_str("</w:tr>"),
            Event::Start(Tag::TableCell) => {
                cell = true;
                body.push_str("<w:tc><w:p>");
                paragraph = true;
            }
            Event::End(TagEnd::TableCell) => {
                close(&mut body, &mut paragraph);
                body.push_str("</w:tc>");
                cell = false;
            }
            Event::Start(Tag::Image { .. }) | Event::Html(_) | Event::InlineHtml(_) => {
                anyhow::bail!(
                    "DOCX export supports Markdown text, headings, lists and tables. Images and raw HTML must be converted before exporting."
                )
            }
            Event::Start(Tag::Strikethrough) => strike = true,
            Event::End(TagEnd::Strikethrough) => strike = false,
            Event::Start(Tag::List(start)) => lists.push(start),
            Event::End(TagEnd::List(_)) => {
                lists.pop();
            }
            Event::Start(Tag::Link { dest_url, .. }) => links.push(dest_url.to_string()),
            Event::End(TagEnd::Link) => {
                if let Some(url) = links.pop() {
                    body.push_str(&format!(
                        "<w:r><w:t xml:space=\"preserve\"> ({})</w:t></w:r>",
                        xml(&url)
                    ));
                }
            }
            Event::Start(Tag::Item) => {
                close(&mut body, &mut paragraph);
                let prefix = match lists.last_mut() {
                    Some(Some(n)) => {
                        let label = format!("{}. ", n);
                        *n += 1;
                        label
                    }
                    _ => "• ".into(),
                };
                body.push_str(&format!(
                    "<w:p><w:r><w:t xml:space=\"preserve\">{prefix}</w:t></w:r>"
                ));
                paragraph = true;
            }
            Event::End(TagEnd::Item) => close(&mut body, &mut paragraph),
            Event::Text(text) | Event::Code(text) => {
                if !paragraph {
                    body.push_str("<w:p>");
                    paragraph = true;
                }
                body.push_str("<w:r><w:rPr>");
                if bold || head {
                    body.push_str("<w:b/>");
                }
                if italic {
                    body.push_str("<w:i/>");
                }
                if strike {
                    body.push_str("<w:strike/>");
                }
                body.push_str("</w:rPr><w:t xml:space=\"preserve\">");
                body.push_str(&xml(&text));
                body.push_str("</w:t></w:r>");
            }
            Event::SoftBreak | Event::HardBreak => {
                if paragraph {
                    body.push_str("<w:r><w:br/></w:r>");
                }
            }
            Event::End(TagEnd::CodeBlock) => close(&mut body, &mut paragraph),
            _ => {}
        }
    }
    close(&mut body, &mut paragraph);
    ensure!(!cell, "Invalid document table");
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>{body}<w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/><w:pgMar w:top=\"720\" w:right=\"720\" w:bottom=\"720\" w:left=\"720\"/></w:sectPr></w:body></w:document>"
    );
    let mut styles = String::from(
        r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Inter" w:hAnsi="Inter"/><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults>"#,
    );
    for i in 1..=6 {
        styles.push_str(&format!("<w:style w:type=\"paragraph\" w:styleId=\"Heading{i}\"><w:name w:val=\"heading {i}\"/><w:pPr><w:spacing w:before=\"240\" w:after=\"120\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"{}\"/></w:rPr></w:style>",36-i*2));
    }
    styles.push_str("</w:styles>");
    let files=[("[Content_Types].xml",r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#.to_owned()),("_rels/.rels",r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_owned()),("word/_rels/document.xml.rels",r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="styles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#.to_owned()),("word/document.xml",document),("word/styles.xml",styles)];
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, content) in files {
        zip.start_file(
            name,
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )?;
        zip.write_all(content.as_bytes())?;
    }
    Ok(zip.finish()?.into_inner())
}
pub fn export(v: &Value) -> Result<Value> {
    let source = v["source"].as_str().unwrap_or("");
    let (extension, mime, bytes) = match v["language"].as_str() {
        Some("markdown") => (
            "docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            docx(source)?,
        ),
        Some("html") => {
            let state = serde_json::to_string(&v["state"])?.replace('<', "\\u003c");
            let boot = format!(
                "<style>:root{{--font-sans:Inter,system-ui,sans-serif;--font-mono:monospace}}body{{font-family:var(--font-sans)}}</style><script>window.kindredArtifact={{ready:Promise.resolve({state}),markDirty(){{}},save:async state=>{{throw new Error('This is a downloaded snapshot. Open the Kindred artifact to save shared edits.')}}}};</script>"
            );
            let lower = source.to_ascii_lowercase();
            let at = lower
                .find("<head")
                .and_then(|start| source[start..].find('>').map(|end| start + end + 1))
                .or_else(|| {
                    lower
                        .find("<!doctype")
                        .and_then(|start| source[start..].find('>').map(|end| start + end + 1))
                })
                .unwrap_or(0);
            let mut html = source.to_owned();
            html.insert_str(at, &boot);
            ("html", "text/html", html.into_bytes())
        }
        _ => (
            "kindred.json",
            "application/json",
            serde_json::to_vec_pretty(v)?,
        ),
    };
    let name: String = v["title"]
        .as_str()
        .unwrap_or("Artifact")
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '-'
            } else {
                c
            }
        })
        .take(100)
        .collect();
    Ok(
        json!({"filename":format!("{name}.{extension}"),"content_type":mime,"revision":v["revision"],"artifact_id":v["id"],"data_base64":STANDARD.encode(bytes)}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    #[test]
    fn snapshots_keep_saved_state_and_document_mode() {
        let v = json!({"title":"A/report","language":"html","revision":7,"source":"<!doctype html><html><head></head><body>Page</body></html>","state":{"text":"</script><script>bad()</script>"}});
        let output = export(&v).unwrap();
        let html = String::from_utf8(
            STANDARD
                .decode(output["data_base64"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(!html.contains("<script>bad()"));
        assert!(html.contains("Promise.resolve"));
        assert_eq!(output["filename"], "A-report.html");
        assert!(docx("![Image](image.png)").is_err());
    }
    #[test]
    fn exports_saved_document_with_formatting_and_safe_xml() {
        let v = json!({"id":"a","title":"Report","language":"markdown","revision":4,"source":"# Report\n\nHuman **correction** & latest.\n\n| Item | State |\n| --- | --- |\n| One | Reviewed |","state":{}});
        let e = export(&v).unwrap();
        assert_eq!(e["revision"], 4);
        let data = STANDARD.decode(e["data_base64"].as_str().unwrap()).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(data)).unwrap();
        let mut xml = String::new();
        archive
            .by_name("word/document.xml")
            .unwrap()
            .read_to_string(&mut xml)
            .unwrap();
        assert!(
            xml.contains("Human ")
                && xml.contains("correction")
                && xml.contains("&amp;")
                && xml.contains("<w:tbl>")
        );
        assert_eq!(archive.len(), 5);
    }
}
