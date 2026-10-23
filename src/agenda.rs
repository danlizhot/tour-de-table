//! Ordre du jour : lecture du texte d'un document Word, PDF, texte ou Markdown.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};

pub const EXTENSIONS: &[&str] = &["docx", "pdf", "txt", "md", "markdown"];

/// Taille maximale conservée : un ordre du jour fait rarement plus de quelques pages.
const MAX_CHARS: usize = 20_000;

pub fn read(path: &Path) -> Result<String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let text = match ext.as_str() {
        "docx" => docx_text(path).with_context(|| format!("Lecture de {}", path.display()))?,
        "pdf" => pdf_extract::extract_text(path).context("Lecture du PDF (est-il numérisé en image?)")?,
        "txt" | "md" | "markdown" => {
            let bytes = std::fs::read(path).with_context(|| format!("Lecture de {}", path.display()))?;
            String::from_utf8(bytes.clone()).unwrap_or_else(|_| latin1(&bytes))
        }
        _ => bail!("Format non pris en charge : utilisez Word (.docx), PDF, .txt ou .md"),
    };
    let text = tidy(&text);
    if text.trim().is_empty() {
        bail!("Aucun texte trouvé dans {}", path.display());
    }
    Ok(text.chars().take(MAX_CHARS).collect())
}

/// Les vieux fichiers texte Windows sont souvent en Windows-1252/Latin-1.
fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// Un .docx est une archive ZIP ; le texte est dans `word/document.xml`.
fn docx_text(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file).context("Fichier Word illisible")?;
    let mut xml = String::new();
    zip.by_name("word/document.xml")
        .context("Fichier Word sans contenu (word/document.xml)")?
        .read_to_string(&mut xml)?;
    let mut numbering = String::new();
    if let Ok(mut f) = zip.by_name("word/numbering.xml") {
        f.read_to_string(&mut numbering)?;
    }
    Ok(docx_xml_to_text(&xml, &bullet_lists(&numbering)))
}

/// Identifiants de listes Word à puces (les autres sont numérotées).
/// `numbering.xml` relie chaque `w:num` à un `w:abstractNum` dont le niveau 0
/// a un format `bullet` ou `decimal`, `upperLetter`…
fn bullet_lists(numbering: &str) -> Vec<String> {
    let attr = |tag: &str, name: &str| -> Option<String> {
        let i = tag.find(&format!("{name}=\""))? + name.len() + 2;
        Some(tag[i..].split('"').next()?.to_string())
    };
    let mut bullet_abstract = Vec::new();
    for block in numbering.split("<w:abstractNum ").skip(1) {
        let id = attr(block, "w:abstractNumId");
        let lvl0 = block.split("<w:lvl ").nth(1).unwrap_or("");
        let fmt = lvl0.split("<w:numFmt ").nth(1).and_then(|t| attr(t, "w:val"));
        if let (Some(id), Some("bullet")) = (id, fmt.as_deref()) {
            bullet_abstract.push(id);
        }
    }
    numbering
        .split("<w:num ")
        .skip(1)
        .filter_map(|block| {
            let num = attr(block, "w:numId")?;
            let abs = block.split("<w:abstractNumId ").nth(1).and_then(|t| attr(t, "w:val"))?;
            bullet_abstract.contains(&abs).then_some(num)
        })
        .collect()
}

/// Extraction minimale : texte des balises `<w:t>`, saut de ligne à chaque
/// paragraphe `</w:p>`, tabulations et sauts `<w:tab/>` `<w:br/>`.
/// Les numéros automatiques des listes Word ne sont pas dans le texte : on
/// ajoute une puce aux paragraphes de liste (`<w:numPr>`).
fn docx_xml_to_text(xml: &str, bullet_lists: &[String]) -> String {
    let mut out = String::new();
    let mut para = String::new();
    let mut in_text = false;
    // Liste du paragraphe courant : (numId, niveau). Les numéros automatiques
    // de Word ne sont pas dans le texte : on les recalcule.
    let mut list: Option<(String, usize)> = None;
    let mut counters: std::collections::HashMap<String, [usize; 9]> = Default::default();
    // `<w:tabs>` déclare des taquets de tabulation : ce n'est pas du texte.
    let mut in_tabs = false;
    let mut rest = xml;
    while let Some(lt) = rest.find('<') {
        if in_text {
            para.push_str(&unescape(&rest[..lt]));
        }
        let Some(gt) = rest[lt..].find('>') else { break };
        let tag = &rest[lt + 1..lt + gt];
        let name = tag.split([' ', '/']).next().unwrap_or("");
        match (tag.starts_with('/'), name) {
            (false, "w:t") => in_text = !tag.ends_with('/'),
            (true, _) if tag == "/w:t" => in_text = false,
            (false, "w:tabs") => in_tabs = true,
            (true, _) if tag == "/w:tabs" => in_tabs = false,
            (false, "w:tab") if !in_tabs => para.push('\t'),
            (false, "w:br") => para.push('\n'),
            (false, "w:numPr") => list = Some((String::new(), 0)),
            (false, "w:ilvl") => {
                if let Some(l) = list.as_mut() {
                    l.1 = val(tag).and_then(|v| v.parse().ok()).unwrap_or(0).min(8);
                }
            }
            (false, "w:numId") => {
                if let Some(l) = list.as_mut() {
                    l.0 = val(tag).unwrap_or_default();
                }
            }
            (true, _) if tag == "/w:p" => {
                if let Some((num, lvl)) = list.take().filter(|_| !para.trim().is_empty()) {
                    out.push_str(&"  ".repeat(lvl));
                    if bullet_lists.contains(&num) || num == "0" {
                        out.push_str("- ");
                    } else {
                        let c = counters.entry(num).or_default();
                        c[lvl] += 1;
                        c[lvl + 1..].iter_mut().for_each(|x| *x = 0);
                        out.push_str(&format!("{}. ", c[lvl]));
                    }
                }
                out.push_str(para.trim_end());
                out.push('\n');
                para.clear();
            }
            _ => {}
        }
        rest = &rest[lt + gt + 1..];
    }
    out
}

fn val(tag: &str) -> Option<String> {
    let i = tag.find("w:val=\"")? + 7;
    Some(tag[i..].split('"').next()?.to_string())
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Supprime les lignes vides en trop et les espaces de fin.
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank = 0;
    for line in text.lines().map(str::trim_end) {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}

/// Version courte pour guider Whisper (sujets, noms propres, acronymes).
pub fn hint(agenda: &str) -> String {
    let joined = agenda
        .lines()
        .map(|l| l.trim().trim_start_matches(|c: char| c == '-' || c == '*' || c == '•' || c.is_ascii_digit() || c == '.' || c == ')').trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ; ");
    joined.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docx_paragraphes_listes_et_entites() {
        let xml = r#"<w:document><w:body>
            <w:p><w:r><w:t>Ordre du jour</w:t></w:r></w:p>
            <w:p><w:pPr><w:tabs><w:tab w:val="left" w:pos="720"/></w:tabs><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t xml:space="preserve">Budget </w:t></w:r><w:r><w:t>2027 &amp; suivi</w:t></w:r></w:p>
            <w:p><w:r><w:t>Varia</w:t><w:tab/><w:t>(10 min)</w:t></w:r></w:p>
            <w:p/>
        </w:body></w:document>"#;
        assert_eq!(docx_xml_to_text(xml, &["1".into()]), "Ordre du jour\n- Budget 2027 & suivi\nVaria\t(10 min)\n");
    }

    #[test]
    fn docx_numerotation_word() {
        let item = |t: &str, lvl: u8| {
            format!(r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="{lvl}"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>{t}</w:t></w:r></w:p>"#)
        };
        let xml = [item("Ouverture", 0), item("Budget", 0), item("Patinoire", 1), item("Parc", 1), item("Varia", 0)].concat();
        let numbering = r#"<w:numbering><w:abstractNum w:abstractNumId="7"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum>
            <w:abstractNum w:abstractNumId="8"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum>
            <w:num w:numId="2"><w:abstractNumId w:val="7"/></w:num><w:num w:numId="3"><w:abstractNumId w:val="8"/></w:num></w:numbering>"#;
        assert_eq!(bullet_lists(numbering), vec!["3".to_string()]);
        assert_eq!(
            docx_xml_to_text(&xml, &bullet_lists(numbering)),
            "1. Ouverture\n2. Budget\n  1. Patinoire\n  2. Parc\n3. Varia\n"
        );
    }

    #[test]
    fn indice_pour_whisper() {
        let a = "Ordre du jour\n1. Ouverture\n2) Projet Hydro-Québec\n- Varia\n";
        assert_eq!(hint(a), "Ordre du jour ; Ouverture ; Projet Hydro-Québec ; Varia");
    }

    #[test]
    fn nettoyage() {
        assert_eq!(tidy("a  \n\n\n\nb\n"), "a\n\nb");
    }
}
