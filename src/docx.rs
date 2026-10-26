//! Compte rendu en Word (.docx), mis en forme comme les comptes rendus d'organisme : Calibri
//! 11 pt, titres en gras, un filet gris avant chaque point, listes à puces, page lettre.
//! Seul le Markdown que produit `summary.rs` est reconnu : titres `#`, `##` et `###`, puces,
//! **gras**, *italique*, citations `>`.

use std::io::Write;
use std::path::Path;

use anyhow::Result;

const CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/><Override PartName="/word/numbering.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"/></Types>"#;

const RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

const DOCUMENT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering" Target="numbering.xml"/></Relationships>"#;

const STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:eastAsia="Calibri" w:cs="Calibri"/><w:sz w:val="22"/><w:szCs w:val="22"/><w:lang w:val="fr-CA"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="160" w:line="259" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style></w:styles>"#;

/// Une seule liste à puces « • », retrait de 1,27 cm.
const NUMBERING: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="0"><w:multiLevelType w:val="hybridMultilevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="•"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="720" w:hanging="360"/></w:pPr></w:lvl><w:lvl w:ilvl="1"><w:start w:val="1"/><w:numFmt w:val="bullet"/><w:lvlText w:val="o"/><w:lvlJc w:val="left"/><w:pPr><w:ind w:left="1440" w:hanging="360"/></w:pPr><w:rPr><w:rFonts w:ascii="Courier New" w:hAnsi="Courier New"/></w:rPr></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num></w:numbering>"#;

/// Lettre, marges de 2,5 cm.
const SECTION: &str = r#"<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1417" w:right="1417" w:bottom="1417" w:left="1417" w:header="708" w:footer="708" w:gutter="0"/></w:sectPr>"#;

/// Écrit le compte rendu `md` en .docx (écriture complète puis renommage).
pub fn ecrire(md: &str, chemin: &Path) -> Result<()> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (nom, contenu) in [
        ("[Content_Types].xml", CONTENT_TYPES.to_string()),
        ("_rels/.rels", RELS.to_string()),
        ("word/_rels/document.xml.rels", DOCUMENT_RELS.to_string()),
        ("word/styles.xml", STYLES.to_string()),
        ("word/numbering.xml", NUMBERING.to_string()),
        ("word/document.xml", document(md)),
    ] {
        zip.start_file(nom, options)?;
        zip.write_all(contenu.as_bytes())?;
    }
    let octets = zip.finish()?.into_inner();
    let tmp = chemin.with_extension("docx.tmp");
    std::fs::write(&tmp, octets)?;
    std::fs::rename(&tmp, chemin)?;
    Ok(())
}

fn document(md: &str) -> String {
    let mut corps = String::new();
    for ligne in md.lines() {
        let t = ligne.trim_end();
        let sans_retrait = t.trim_start();
        if sans_retrait.is_empty() || sans_retrait.starts_with("<!--") {
            continue;
        }
        if let Some(titre) = t.strip_prefix("# ") {
            corps.push_str(&paragraphe("", &format!("**{}**", titre.to_uppercase())));
        } else if let Some(titre) = t.strip_prefix("## ") {
            // Filet avant chaque point et la levée de la séance, comme dans les modèles reçus.
            if !titre.starts_with("Présences") {
                corps.push_str(FILET);
            }
            corps.push_str(&paragraphe("", &format!("**{}**", titre.replace("**", ""))));
        } else if let Some(titre) = t.strip_prefix("### ") {
            corps.push_str(&paragraphe("", &format!("**{}**", titre.replace("**", ""))));
        } else if let Some(puce) = sans_retrait.strip_prefix("- ").or_else(|| sans_retrait.strip_prefix("* ")) {
            let niveau = usize::from(t.len() - sans_retrait.len() >= 2);
            let ppr = format!(r#"<w:numPr><w:ilvl w:val="{niveau}"/><w:numId w:val="1"/></w:numPr><w:spacing w:after="0"/>"#);
            corps.push_str(&paragraphe(&ppr, puce));
        } else if let Some(citation) = sans_retrait.strip_prefix("> ") {
            corps.push_str(&paragraphe(r#"<w:ind w:left="360"/>"#, &format!("*{citation}*")));
        } else if t.starts_with("**Date**") {
            // « **Date** : … · **Heure** : … » : une ligne chacun.
            corps.push_str(&paragraphe("", &t.replace(" · ", "\n")));
        } else {
            corps.push_str(&paragraphe("", sans_retrait));
        }
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{corps}{SECTION}</w:body></w:document>"#
    )
}

const FILET: &str = r#"<w:p><w:pPr><w:pBdr><w:bottom w:val="single" w:sz="6" w:space="1" w:color="A0A0A0"/></w:pBdr></w:pPr></w:p>"#;

fn paragraphe(ppr: &str, texte: &str) -> String {
    let ppr = if ppr.is_empty() { String::new() } else { format!("<w:pPr>{ppr}</w:pPr>") };
    format!("<w:p>{ppr}{}</w:p>", segments(texte))
}

/// Segments **gras** et *italique* d'une ligne ; « \n » devient un saut de ligne.
fn segments(texte: &str) -> String {
    let mut runs = String::new();
    for (i, gras) in texte.split("**").enumerate() {
        for (j, partie) in gras.split('*').enumerate() {
            if partie.is_empty() {
                continue;
            }
            let mut rpr = String::new();
            if i % 2 == 1 {
                rpr.push_str("<w:b/><w:bCs/>");
            }
            if j % 2 == 1 {
                rpr.push_str("<w:i/><w:iCs/>");
            }
            let rpr = if rpr.is_empty() { String::new() } else { format!("<w:rPr>{rpr}</w:rPr>") };
            let texte = partie.split('\n').map(|l| format!(r#"<w:t xml:space="preserve">{}</w:t>"#, echapper(l))).collect::<Vec<_>>().join("<w:br/>");
            runs.push_str(&format!("<w:r>{rpr}{texte}</w:r>"));
        }
    }
    runs
}

fn echapper(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mise_en_forme() {
        let md = "# Compte rendu\n\n**Rencontre de l'équipe**\n\n**Date** : 8 octobre 2026 · **Heure** : 11 h 00\n\n\
                  ## Présences\n\n- Julie Roy\n\n*Liste à vérifier.*\n\n## 1. Budget & dépenses\n\nMme Roy présente <le> budget.\n\
                  - Budget de 2 500 $.\n  - Sous-point.\n\n> **À vérifier** : 300";
        let doc = document(md);
        assert!(doc.contains(r#"<w:b/><w:bCs/></w:rPr><w:t xml:space="preserve">COMPTE RENDU</w:t>"#), "{doc}");
        assert!(doc.contains(r#"<w:t xml:space="preserve">Date</w:t></w:r><w:r><w:t xml:space="preserve"> : 8 octobre 2026</w:t><w:br/>"#), "{doc}");
        // Filet avant le point, pas avant les présences.
        assert_eq!(doc.matches("<w:pBdr>").count(), 1);
        assert!(doc.find("w:pBdr").unwrap() > doc.find("Julie Roy").unwrap());
        assert!(doc.contains("1. Budget &amp; dépenses") && doc.contains("présente &lt;le&gt; budget"));
        assert!(doc.contains(r#"<w:ilvl w:val="0"/><w:numId w:val="1"/>"#) && doc.contains(r#"<w:ilvl w:val="1"/>"#));
        assert!(doc.contains(r#"<w:i/><w:iCs/></w:rPr><w:t xml:space="preserve">Liste à vérifier.</w:t>"#), "{doc}");
        assert!(doc.contains(r#"<w:b/><w:bCs/><w:i/><w:iCs/></w:rPr><w:t xml:space="preserve">À vérifier</w:t>"#), "{doc}");

        let dir = std::env::temp_dir().join(format!("tourdetable-test-{}-docx", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("compte-rendu.docx");
        ecrire(md, &f).unwrap();
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&f).unwrap()).unwrap();
        for nom in ["[Content_Types].xml", "_rels/.rels", "word/document.xml", "word/styles.xml", "word/numbering.xml"] {
            assert!(zip.by_name(nom).is_ok(), "{nom}");
        }
        // Relu par le lecteur des ordres du jour : le texte y est.
        let texte = crate::agenda::read(&f).unwrap();
        assert!(texte.contains("Mme Roy présente <le> budget.") && texte.contains("Budget de 2 500 $."), "{texte}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
