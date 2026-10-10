//! Native sheets export as an OOXML workbook, preserving formula text and styles.
use super::xml;
use anyhow::{Result, ensure};
use serde_json::Value;
use std::{collections::HashMap, io::{Cursor, Write}};
fn column(mut n:usize)->String{let mut s=String::new();n+=1;while n>0{s.insert(0,(b'A'+((n-1)%26)as u8)as char);n=(n-1)/26;}s}
fn color(v:&Value)->Option<String>{let s=v.as_str()?.strip_prefix('#')?;if !s.bytes().all(|c|c.is_ascii_hexdigit()){return None;}match s.len(){6=>Some(format!("FF{}",s.to_uppercase())),3=>Some(format!("FF{}",s.chars().flat_map(|c|[c,c]).collect::<String>().to_uppercase())),_=>None}}
fn intern(value:String,values:&mut Vec<String>,ids:&mut HashMap<String,usize>)->usize{if let Some(id)=ids.get(&value){return *id;}let id=values.len();ids.insert(value.clone(),id);values.push(value);id}
pub(super) fn workbook(v:&Value)->Result<Vec<u8>>{
 let rows=v["state"]["rows"].as_array().ok_or_else(||anyhow::anyhow!("Sheet rows are missing"))?;
 let cols=rows.iter().filter_map(Value::as_array).map(Vec::len).max().unwrap_or(0);
 ensure!(rows.len().saturating_mul(cols)<=10000,"Sheet export supports up to 10,000 cells");
 let mut fonts:Vec<String>=vec!["<font><sz val=\"12\"/><name val=\"Inter\"/></font>".into()];let mut font_ids=HashMap::from([(fonts[0].clone(),0)]);
 let mut fills=vec!["<fill><patternFill patternType=\"none\"/></fill>".into(),"<fill><patternFill patternType=\"gray125\"/></fill>".into()];let mut fill_ids=HashMap::new();
 let mut formats=vec!["<xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/>".into()];let mut format_ids=HashMap::new();
 let mut data=String::new();
 for(i,row)in rows.iter().enumerate(){let row=row.as_array().ok_or_else(||anyhow::anyhow!("Each sheet row must be an array"))?;data.push_str(&format!("<row r=\"{}\">",i+1));
  for(j,cell)in row.iter().enumerate(){
   ensure!(cell.is_null()||cell.is_string()||cell.is_number()||cell.is_boolean(),"Sheet cells must contain text, numbers or booleans");
   let text=if let Some(s)=cell.as_str(){s.to_owned()}else if cell.is_null(){String::new()}else{cell.to_string()};let style=&v["state"]["cellStyles"][format!("{i}:{j}")];
   let family=xml(style["fontFamily"].as_str().unwrap_or("Inter"));let size=style["fontSize"].as_str().and_then(|s|s.trim_end_matches("px").parse::<f64>().ok()).filter(|n|n.is_finite()).unwrap_or(16.).clamp(6.,200.)*0.75;
   let mut font=format!("<font><sz val=\"{size}\"/><name val=\"{family}\"/>");if style["fontWeight"]=="700"{font.push_str("<b/>");}if style["fontStyle"]=="italic"{font.push_str("<i/>");}if let Some(c)=color(&style["color"]){font.push_str(&format!("<color rgb=\"{c}\"/>"));}font.push_str("</font>");let font=intern(font,&mut fonts,&mut font_ids);
   let fill=if let Some(c)=color(&style["backgroundColor"]){intern(format!("<fill><patternFill patternType=\"solid\"><fgColor rgb=\"{c}\"/><bgColor indexed=\"64\"/></patternFill></fill>"),&mut fills,&mut fill_ids)}else{0};
   let align=style["textAlign"].as_str().filter(|s|["left","center","right"].contains(s)).unwrap_or("general");
   let format=intern(format!("<xf numFmtId=\"0\" fontId=\"{font}\" fillId=\"{fill}\" borderId=\"0\" xfId=\"0\" applyFont=\"1\" applyFill=\"1\" applyAlignment=\"1\"><alignment horizontal=\"{align}\" vertical=\"top\" wrapText=\"1\"/></xf>"),&mut formats,&mut format_ids);
   let address=format!("{}{}",column(j),i+1);
   if let Some(formula)=text.strip_prefix('=') {data.push_str(&format!("<c r=\"{address}\" s=\"{format}\"><f>{}</f></c>",xml(formula)));}
   else if cell.is_boolean(){data.push_str(&format!("<c r=\"{address}\" s=\"{format}\" t=\"b\"><v>{}</v></c>",u8::from(cell.as_bool().unwrap())));}
   else{
    // Keep leading zeros, long account numbers and whitespace as text. Explicit
    // apostrophe prefixes also request text, as in Excel's own cell editor.
    let canonical=text.trim()==text&&!text.starts_with('\'')&&!text.starts_with('+')&&text.chars().filter(char::is_ascii_digit).count()<=15;
    let digits=text.trim_start_matches('-');let leading_zero=digits.starts_with('0')&&digits.as_bytes().get(1).is_some_and(u8::is_ascii_digit);
    let numeric=canonical&&!leading_zero&&!text.is_empty()&&text.parse::<f64>().is_ok_and(|n|n.is_finite());
    if numeric{data.push_str(&format!("<c r=\"{address}\" s=\"{format}\"><v>{}</v></c>",xml(&text)));}
    else{data.push_str(&format!("<c r=\"{address}\" s=\"{format}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>",xml(text.strip_prefix('\'').unwrap_or(&text))));}
   }
  }data.push_str("</row>");
 }
 let styles=format!(r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="{}">{}</fonts><fills count="{}">{}</fills><borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="{}">{}</cellXfs><cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles></styleSheet>"#,fonts.len(),fonts.join(""),fills.len(),fills.join(""),formats.len(),formats.join(""));
 let worksheet=format!(r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetViews><sheetView workbookViewId="0"/></sheetViews><sheetFormatPr defaultColWidth="18" defaultRowHeight="20"/><sheetData>{data}</sheetData></worksheet>"#);
 let files=[
 ("[Content_Types].xml",r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#.to_owned()),
 ("_rels/.rels",r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="workbook" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.to_owned()),
 ("xl/workbook.xml",r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Sheet1" sheetId="1" r:id="sheet1"/></sheets><calcPr calcId="191029" fullCalcOnLoad="1" forceFullCalc="1"/></workbook>"#.to_owned()),
 ("xl/_rels/workbook.xml.rels",r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="sheet1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="styles" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#.to_owned()),("xl/styles.xml",styles),("xl/worksheets/sheet1.xml",worksheet)];
 let mut zip=zip::ZipWriter::new(Cursor::new(Vec::new()));for(name,content)in files{zip.start_file(name,zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored))?;zip.write_all(content.as_bytes())?;}Ok(zip.finish()?.into_inner())
}
