//! Revision-specific exports from the saved artifact, never a stale chat copy.
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::io::{Cursor, Write};
#[path = "spreadsheet_export.rs"]
mod spreadsheet;
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
    word_package(&body)
}
fn word_package(body: &str) -> Result<Vec<u8>> { word_package_options(body, None, &[]) }
fn word_package_options(body: &str, page:Option<&Value>, links:&[String]) -> Result<Vec<u8>> {
    let (mut width,mut height)=if page.is_some_and(|p|p["size"]=="a4"){(11906,16838)}else{(12240,15840)};
    let landscape=page.is_some_and(|p|p["orientation"]=="landscape");if landscape{std::mem::swap(&mut width,&mut height);}
    let orientation=if landscape{" w:orient=\"landscape\""}else{""};
    let margin=if page.is_some_and(|p|p["margins"]=="normal"){1440}else{720};

    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><w:body>{body}<w:sectPr><w:pgSz w:w=\"{width}\" w:h=\"{height}\"{orientation}/><w:pgMar w:top=\"{margin}\" w:right=\"{margin}\" w:bottom=\"{margin}\" w:left=\"{margin}\"/></w:sectPr></w:body></w:document>"
    );
    let mut styles = String::from(
        r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Inter" w:hAnsi="Inter"/><w:sz w:val="22"/></w:rPr></w:rPrDefault></w:docDefaults>"#,
    );
    for i in 1..=6 {
        styles.push_str(&format!("<w:style w:type=\"paragraph\" w:styleId=\"Heading{i}\"><w:name w:val=\"heading {i}\"/><w:pPr><w:spacing w:before=\"240\" w:after=\"120\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"{}\"/></w:rPr></w:style>",36-i*2));
    }
    styles.push_str("</w:styles>");
    let mut relationships=r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="styles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>"#.to_owned();
    for(i,href)in links.iter().enumerate(){relationships.push_str(&format!(r#"<Relationship Id="link{i}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="{}" TargetMode="External"/>"#,xml(href)));}relationships.push_str("</Relationships>");
    let files=[("[Content_Types].xml",r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#.to_owned()),("_rels/.rels",r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#.to_owned()),("word/_rels/document.xml.rels",relationships),("word/document.xml",document),("word/styles.xml",styles)];
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

fn rich_runs(v: &Value, links:&mut Vec<String>, depth:usize) -> Result<String> {
    ensure!(depth<64,"Document nesting is too deep");
    if v["type"]=="hardBreak" {return Ok("<w:r><w:br/></w:r>".into());}
    if v["type"]=="text" {
        let mut props=String::new();
        for mark in v["marks"].as_array().into_iter().flatten() {
            match mark["type"].as_str().unwrap_or("") {
                "bold"=>props.push_str("<w:b/>"), "italic"=>props.push_str("<w:i/>"),
                "underline"=>props.push_str("<w:u w:val=\"single\"/>"), "strike"=>props.push_str("<w:strike/>"),
                "textStyle"=>{
                    let a=&mark["attrs"];
                    if let Some(font)=a["fontFamily"].as_str(){props.push_str(&format!("<w:rFonts w:ascii=\"{}\" w:hAnsi=\"{}\"/>",xml(font),xml(font)));}
                    if let Some(size)=a["fontSize"].as_str().and_then(|s|s.trim_end_matches("px").parse::<f64>().ok()).filter(|n|n.is_finite()) {props.push_str(&format!("<w:sz w:val=\"{}\"/>",(size.clamp(6.,200.)*1.5).round() as u32));}
                    if let Some(c)=a["color"].as_str().and_then(|c|c.strip_prefix('#')).filter(|c|c.len()==6&&c.bytes().all(|b|b.is_ascii_hexdigit())){props.push_str(&format!("<w:color w:val=\"{c}\"/>"));}
                },_=>{}
            }
        }
        let run=format!("<w:r><w:rPr>{props}</w:rPr><w:t xml:space=\"preserve\">{}</w:t></w:r>",xml(v["text"].as_str().unwrap_or("")));
        if let Some(href)=v["marks"].as_array().into_iter().flatten().find(|m|m["type"]=="link").and_then(|m|m["attrs"]["href"].as_str()).filter(|h|{let s=h.to_ascii_lowercase();s.starts_with("https://")||s.starts_with("http://")||s.starts_with("mailto:")}){
            let index=links.iter().position(|h|h==href).unwrap_or_else(||{links.push(href.to_owned());links.len()-1});return Ok(format!("<w:hyperlink r:id=\"link{index}\">{run}</w:hyperlink>"));
        }
        return Ok(run);
    }
    let mut result=String::new();for child in v["content"].as_array().into_iter().flatten(){result.push_str(&rich_runs(child,links,depth+1)?);}Ok(result)
}
fn rich_blocks(v: &Value, depth: usize, links:&mut Vec<String>) -> Result<String> {
    ensure!(depth<64,"Document nesting is too deep");
    let children=v["content"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let mut body=String::new();
    match v["type"].as_str().unwrap_or("") {
        "paragraph"|"heading"|"codeBlock"=>{
            let mut props=String::new();
            if v["type"]=="heading" {let level=v["attrs"]["level"].as_u64().unwrap_or(1).clamp(1,6);props.push_str(&format!("<w:pStyle w:val=\"Heading{level}\"/>"));}
            if let Some(align)=v["attrs"]["textAlign"].as_str().filter(|s|["left","center","right","justify"].contains(s)){props.push_str(&format!("<w:jc w:val=\"{}\"/>",if align=="justify"{"both"}else{align}));}
            body=format!("<w:p><w:pPr>{props}</w:pPr>{}</w:p>",rich_runs(v,links,depth+1)?);
        },
        "table"=>{body.push_str("<w:tbl><w:tblPr><w:tblBorders><w:top w:val=\"single\" w:sz=\"4\"/><w:left w:val=\"single\" w:sz=\"4\"/><w:bottom w:val=\"single\" w:sz=\"4\"/><w:right w:val=\"single\" w:sz=\"4\"/><w:insideH w:val=\"single\" w:sz=\"4\"/><w:insideV w:val=\"single\" w:sz=\"4\"/></w:tblBorders></w:tblPr>");for c in children{body.push_str(&rich_blocks(c,depth+1,links)?);}body.push_str("</w:tbl>");},
        "tableRow"=>{body.push_str("<w:tr>");for c in children{body.push_str(&rich_blocks(c,depth+1,links)?);}body.push_str("</w:tr>");},
        "tableCell"|"tableHeader"=>{body.push_str("<w:tc>");for c in children{body.push_str(&rich_blocks(c,depth+1,links)?);}if children.is_empty()||children.last().is_some_and(|c|c["type"]=="table"){body.push_str("<w:p/>");}body.push_str("</w:tc>");},
        "bulletList"|"orderedList"=>{let start=v["attrs"]["start"].as_u64().unwrap_or(1);for (i,c) in children.iter().enumerate(){let mut item=rich_blocks(c,depth+1,links)?;let prefix=if v["type"]=="orderedList"{format!("{}. ",start+i as u64)}else{"• ".into()};let run=format!("<w:r><w:t xml:space=\"preserve\">{prefix}</w:t></w:r>");if let Some(at)=item.find("</w:pPr>"){item.insert_str(at+"</w:pPr>".len(),&run);}body.push_str(&item);}},
        "image"=>anyhow::bail!("This document contains an image. DOCX export cannot preserve it yet; the saved Kindred document is unchanged."),
        "horizontalRule"=>body="<w:p><w:pPr><w:pBdr><w:bottom w:val=\"single\" w:sz=\"4\"/></w:pBdr></w:pPr></w:p>".into(),
        _=>for c in children{body.push_str(&rich_blocks(c,depth+1,links)?);}
    }
    Ok(body)
}
pub fn export(v: &Value) -> Result<Value> {
    let source = v["source"].as_str().unwrap_or("");
    let (extension, mime, bytes) = match v["language"].as_str() {
        Some("html") if v["kind"]=="sheet" && v["state"]["rows"].is_array() && (source.contains("kindred-sheet-v1") || source.contains("let rows=[];kindredArtifact.ready")) => (
            "xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", spreadsheet::workbook(v)?,
        ),
        Some("markdown") => (
            "docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            docx(source)?,
        ),
        Some("html") if source.starts_with("<!--kindred-document-v1-->") && v["state"]["kindredDocument"]["html"].as_str().is_some_and(|html|source.ends_with(&format!("<main>{html}</main>"))) => (
            "docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            {let mut links=Vec::new();let body=rich_blocks(&v["state"]["kindredDocument"]["document"],0,&mut links)?;word_package_options(&body,Some(&v["state"]["kindredDocument"]["page"]),&links)?},
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
    fn rich_document_keeps_formatting_and_revision_in_docx() {
        use std::io::Read;
        let html="<p>Saved & edited</p>";
        let document=json!({"type":"doc","content":[{"type":"paragraph","attrs":{"textAlign":"center"},"content":[{"type":"text","text":"Saved & edited","marks":[{"type":"bold"},{"type":"underline"},{"type":"textStyle","attrs":{"fontFamily":"Georgia","fontSize":"20px","color":"#123456"}}]}]},{"type":"orderedList","content":[{"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"One"}]}]}]}]});
        let mut v=json!({"kind":"document","title":"Report","language":"html","revision":9,"source":format!("<!--kindred-document-v1--><style></style><main>{html}</main>"),"state":{"kindredDocument":{"version":1,"html":html,"document":document}}});
        let result=export(&v).unwrap();assert_eq!(result["filename"],"Report.docx");assert_eq!(result["revision"],9);
        let bytes=STANDARD.decode(result["data_base64"].as_str().unwrap()).unwrap();
        let mut zip=zip::ZipArchive::new(Cursor::new(bytes)).unwrap();let mut body=String::new();zip.by_name("word/document.xml").unwrap().read_to_string(&mut body).unwrap();
        for part in ["Saved &amp; edited","w:ascii=\"Georgia\"","w:sz w:val=\"30\"","w:color w:val=\"123456\"","w:jc w:val=\"center\"","<w:b/>","<w:u w:val=\"single\"/>","</w:pPr><w:r><w:t xml:space=\"preserve\">1. </w:t>"] {assert!(body.contains(part),"Missing {part}: {body}");}
        // A bot/source update must never download stale rich-editor state.
        v["source"]=json!("<!--kindred-document-v1--><main>New source</main>");assert_eq!(export(&v).unwrap()["filename"],"Report.html");
    }
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

#[cfg(test)]
mod office_export_tests {
 use super::*;
 use std::io::Read;
 fn part(bytes:&[u8],name:&str)->String{let mut zip=zip::ZipArchive::new(Cursor::new(bytes)).unwrap();let mut out=String::new();zip.by_name(name).unwrap().read_to_string(&mut out).unwrap();out}
 fn bytes(v:&Value)->Vec<u8>{STANDARD.decode(v["data_base64"].as_str().unwrap()).unwrap()}
 fn save(name:&str,data:&[u8]){if let Ok(dir)=std::env::var("KINDRED_OFFICE_TEST_OUTPUT"){std::fs::create_dir_all(&dir).unwrap();std::fs::write(std::path::Path::new(&dir).join(name),data).unwrap();}}
 #[test]
 fn office_sheet_exports_cells_formulas_styles_and_literal_identifiers(){
  let v=json!({"id":"sheet","title":"Budget","kind":"sheet","language":"html","revision":4,"source":"<!--kindred-sheet-v1-->","state":{"rows":[["Item","Cost","Tax","Total"],["Hosting","120","=B2*20%","=SUM(B2:C2)"],["00123","'=1+1","<script>& \"quoted\"","12345678901234567890"]],"cellStyles":{"0:0":{"fontWeight":"700","backgroundColor":"#123456","color":"#fff"},"1:1":{"fontFamily":"Georgia","fontSize":"20px","textAlign":"right"}}}});
  let out=export(&v).unwrap();assert_eq!(out["filename"],"Budget.xlsx");assert_eq!(out["revision"],4);let b=bytes(&out);let cells=part(&b,"xl/worksheets/sheet1.xml");let styles=part(&b,"xl/styles.xml");
  for expected in ["<f>B2*20%</f>","<f>SUM(B2:C2)</f>","<v>120</v>",">00123</t>",">=1+1</t>","&lt;script&gt;&amp;",">12345678901234567890</t>"]{assert!(cells.contains(expected),"{expected}: {cells}");}
  for expected in ["FF123456","FFFFFFFF","<b/>","Georgia","<sz val=\"15\"/>","horizontal=\"right\""]{assert!(styles.contains(expected),"{expected}");}
  assert!(part(&b,"xl/workbook.xml").contains("fullCalcOnLoad=\"1\""));save("Budget.xlsx",&b);
  let mut custom=v.clone();custom["source"]=json!("<p>Custom sheet app</p>");assert_eq!(export(&custom).unwrap()["filename"],"Budget.html");
 }
 #[test]
 fn office_document_exports_page_settings_and_real_links(){
  let html="<p>Review the report</p>";let v=json!({"title":"Report","language":"html","kind":"document","revision":3,"source":format!("<!--kindred-document-v1--><style></style><main>{html}</main>"),"state":{"kindredDocument":{"html":html,"page":{"size":"a4","orientation":"landscape","margins":"normal"},"document":{"type":"doc","content":[{"type":"heading","attrs":{"level":1},"content":[{"type":"text","text":"Review the report","marks":[{"type":"bold"},{"type":"link","attrs":{"href":"https://example.com/report?a=1&b=2"}}]}]}]}}}});
  let out=export(&v).unwrap();let b=bytes(&out);let doc=part(&b,"word/document.xml");let rels=part(&b,"word/_rels/document.xml.rels");
  for expected in ["w:w=\"16838\" w:h=\"11906\"","w:orient=\"landscape\"","w:top=\"1440\"","<w:hyperlink r:id=\"link0\"","Review the report"]{assert!(doc.contains(expected),"{expected}: {doc}");}assert!(rels.contains("https://example.com/report?a=1&amp;b=2"));save("Report.docx",&b);
 }
 #[test]
 fn office_document_does_not_silently_drop_images(){
  let v=json!({"language":"html","source":"<!--kindred-document-v1--><main><img></main>","state":{"kindredDocument":{"html":"<img>","document":{"type":"doc","content":[{"type":"image","attrs":{"src":"https://example.com/image.png"}}]}}}});
  assert!(export(&v).unwrap_err().to_string().contains("cannot preserve"));
 }
}
