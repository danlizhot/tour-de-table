//! Ordre du jour : lecture du texte d'un document Word, PDF, texte ou Markdown.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

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

/// Un point de l'ordre du jour. `numero` est recalculé d'après la position (« 1 », « 2.1 »…)
/// par [`renumeroter`] ; `niveau` vaut 1 pour un point principal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub numero: String,
    pub titre: String,
    pub niveau: u8,
    /// Lignes non numérotées sous le point (précisions, durée, responsable…).
    #[serde(default)]
    pub detail: String,
}

/// Genre de numérotation d'une ligne. `Arabe(n)` : « 1. », « 2.1 », « Point 3 : » (n composantes).
#[derive(Clone, Copy, PartialEq)]
enum Genre {
    Arabe(usize),
    Romain,
    Lettre,
    Puce,
}

/// Points de l'ordre du jour reconnus dans le texte. Le niveau se déduit de l'imbrication des
/// genres de numérotation et du retrait (comme la sortie de [`docx_xml_to_text`]) : un genre
/// déjà vu reprend son niveau, un nouveau genre (ou un retrait plus grand) descend d'un niveau.
pub fn points(text: &str) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::new();
    // Chemin courant : (genre, retrait) de chaque niveau ouvert.
    let mut pile: Vec<(Genre, usize)> = Vec::new();
    for ligne in text.lines() {
        let retrait: usize = ligne.chars().take_while(|c| c.is_whitespace()).map(|c| if c == '\t' { 4 } else { 1 }).sum();
        let ligne = ligne.trim();
        if ligne.is_empty() {
            continue;
        }
        let Some((genre, titre)) = marque(ligne).filter(|(_, t)| !t.is_empty() && !est_entete(t)) else {
            // Ligne ordinaire : précision du dernier point (les en-têtes avant le premier sont ignorés).
            if let Some(p) = out.last_mut() {
                if !p.detail.is_empty() {
                    p.detail.push('\n');
                }
                p.detail.push_str(ligne);
            }
            continue;
        };
        let cle = (genre, retrait);
        match pile.iter().position(|&k| k == cle) {
            Some(i) => pile.truncate(i + 1),
            None => {
                while pile.last().is_some_and(|&(_, r)| r > retrait) {
                    pile.pop();
                }
                pile.push(cle);
            }
        }
        // Espaces multiples d'un PDF justifié (« Tour  de  table ») réduits à une seule.
        let titre = titre.split_whitespace().collect::<Vec<_>>().join(" ");
        out.push(Point { numero: String::new(), titre, niveau: pile.len().min(9) as u8, detail: String::new() });
    }
    renumeroter(&mut out);
    out
}

/// Numéros hiérarchiques d'après l'ordre et le niveau : 1, 2, 2.1, 2.2, 3…
pub fn renumeroter(points: &mut [Point]) {
    let mut compteurs = [0usize; 9];
    let mut precedent = 0usize;
    for p in points {
        // Un sous-point sans parent (premier point de niveau 2) est remonté d'un cran.
        let niveau = (p.niveau.max(1) as usize).min(precedent + 1).min(9);
        p.niveau = niveau as u8;
        compteurs[niveau - 1] += 1;
        compteurs[niveau..].iter_mut().for_each(|c| *c = 0);
        p.numero = compteurs[..niveau].iter().map(|c| c.to_string()).collect::<Vec<_>>().join(".");
        precedent = niveau;
    }
}

/// Numérotation en tête de ligne et reste de la ligne.
fn marque(ligne: &str) -> Option<(Genre, &str)> {
    if let Some(reste) = ligne.strip_prefix(['-', '*', '•', '–', '▪', '◦']) {
        return reste.starts_with(char::is_whitespace).then(|| (Genre::Puce, reste.trim()));
    }
    // « Point 1 : », « Point 2.1 - »
    let (reste, point) = match ligne.get(..5) {
        Some(p) if p.eq_ignore_ascii_case("point") && ligne[5..].starts_with(char::is_whitespace) => (ligne[5..].trim_start(), true),
        _ => (ligne, false),
    };
    let chiffres = reste.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(reste.len());
    let nombre = reste[..chiffres].trim_end_matches('.');
    let composantes: Vec<&str> = nombre.split('.').collect();
    if !nombre.is_empty() && composantes.iter().all(|c| !c.is_empty() && c.len() <= 2) {
        let apres = &reste[chiffres..];
        let pointe = reste[..chiffres].ends_with('.');
        let titre = if let Some(t) = apres.trim_start().strip_prefix([':', ')', '-', '–', '—']) {
            // « 1 - Titre » mais pas « 2026-10-30 » ni « 1-800 »
            (apres.starts_with(char::is_whitespace) || !apres.trim_start().starts_with(['-', '–']) || t.starts_with(char::is_whitespace))
                .then_some(t)
        } else if pointe || composantes.len() > 1 || point {
            // « 1. Titre », « 2.1 Titre », « Point 3 Titre »
            apres.starts_with(char::is_whitespace).then_some(apres)
        } else {
            None
        };
        return titre.map(|t| (Genre::Arabe(composantes.len()), t.trim()));
    }
    if point {
        return None;
    }
    // « I. », « IV) », « a) », « B. » (mais pas « M. Tremblay »)
    let lettres = ligne.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(ligne.len());
    let (tete, apres) = ligne.split_at(lettres);
    let apres = apres.strip_prefix(['.', ')'])?;
    if !apres.starts_with(char::is_whitespace) {
        return None;
    }
    let romain = tete.len() <= 6 && tete.chars().all(|c| "IVXLC".contains(c));
    if romain {
        Some((Genre::Romain, apres.trim()))
    } else if tete.len() == 1 && (ligne.as_bytes()[1] == b')' || tete != "M") {
        Some((Genre::Lettre, apres.trim()))
    } else {
        None
    }
}

/// En-têtes d'un ordre du jour qui ne sont pas des points : titre, date.
fn est_entete(titre: &str) -> bool {
    let t = titre.to_lowercase();
    const MOIS: [&str; 12] =
        ["janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre"];
    let annee = t.split(|c: char| !c.is_ascii_digit()).any(|n| n.len() == 4);
    t.starts_with("ordre du jour") || (annee && (MOIS.iter().any(|m| t.contains(m)) || t.chars().all(|c| c.is_ascii_digit() || "-/ ".contains(c))))
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

    fn resume(points: &[Point]) -> Vec<(String, String, u8)> {
        points.iter().map(|p| (p.numero.clone(), p.titre.clone(), p.niveau)).collect()
    }

    fn p(numero: &str, titre: &str, niveau: u8) -> (String, String, u8) {
        (numero.into(), titre.into(), niveau)
    }

    #[test]
    fn points_numerotations_diverses() {
        let a = "Ordre du jour\nRencontre du 30 octobre 2026\n2026-10-30\n1. Ouverture\n2) Adoption du procès-verbal\n3 - Budget 2027\nPoint 4 : Patinoire\n5. Varia\n";
        assert_eq!(
            resume(&points(a)),
            vec![p("1", "Ouverture", 1), p("2", "Adoption du procès-verbal", 1), p("3", "Budget 2027", 1), p("4", "Patinoire", 1), p("5", "Varia", 1)]
        );
        let romains = "I. Ouverture\n  a) Mot de bienvenue\n  b) Présences\nII. Finances\nA. Budget\nB. États financiers\nIII. Levée";
        assert_eq!(
            resume(&points(romains)),
            vec![
                p("1", "Ouverture", 1),
                p("1.1", "Mot de bienvenue", 2),
                p("1.2", "Présences", 2),
                p("2", "Finances", 1),
                p("2.1", "Budget", 2),
                p("2.2", "États financiers", 2),
                p("3", "Levée", 1),
            ]
        );
        assert_eq!(resume(&points("1. Budget\n1.1 Patinoire\n1.2. Parc\n2. Varia")), vec![
            p("1", "Budget", 1),
            p("1.1", "Patinoire", 2),
            p("1.2", "Parc", 2),
            p("2", "Varia", 1)
        ]);
        // Sans numéros, des puces font des points.
        assert_eq!(resume(&points("Ordre du jour\n- Ouverture\n- Varia")), vec![p("1", "Ouverture", 1), p("2", "Varia", 1)]);
        // Puces sous un point numéroté : sous-points.
        assert_eq!(resume(&points("1. Budget\n- Patinoire\n2. Varia")), vec![p("1", "Budget", 1), p("1.1", "Patinoire", 2), p("2", "Varia", 1)]);
    }

    #[test]
    fn points_pas_de_faux_positifs() {
        assert!(points("Ordre du jour\n2026-10-30\nM. Tremblay préside.\n19 h 00 au bureau\n1-800-555-1234\n1,5 M$ investis").is_empty());
        assert!(points("Point de presse demain").is_empty());
        // PDF justifié : espaces multiples réduits.
        assert_eq!(resume(&points("1. Tour  de  table\n7.    Retour sur la rencontre RH")), vec![p("1", "Tour de table", 1), p("2", "Retour sur la rencontre RH", 1)]);
    }

    #[test]
    fn points_et_details() {
        let a = "Ordre du jour\n1. Ouverture\n(5 min)\n2. Budget\nPrésenté par Julie\nDocuments joints\n";
        let v = points(a);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].detail, "(5 min)");
        assert_eq!(v[1].detail, "Présenté par Julie\nDocuments joints");
    }

    #[test]
    fn points_de_la_sortie_word() {
        let item = |t: &str, lvl: u8| {
            format!(r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="{lvl}"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>{t}</w:t></w:r></w:p>"#)
        };
        let xml = [item("Ouverture", 0), item("Budget", 0), item("Patinoire", 1), item("Parc", 1), item("Varia", 0)].concat();
        let texte = docx_xml_to_text(&xml, &[]);
        assert_eq!(
            resume(&points(&texte)),
            vec![p("1", "Ouverture", 1), p("2", "Budget", 1), p("2.1", "Patinoire", 2), p("2.2", "Parc", 2), p("3", "Varia", 1)]
        );
        // Premier test de lecture Word : en-tête, puce, ligne ordinaire.
        let xml = r#"<w:p><w:r><w:t>Ordre du jour</w:t></w:r></w:p><w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>Budget 2027</w:t></w:r></w:p><w:p><w:r><w:t>Varia</w:t><w:tab/><w:t>(10 min)</w:t></w:r></w:p>"#;
        let v = points(&docx_xml_to_text(xml, &["1".into()]));
        assert_eq!(resume(&v), vec![p("1", "Budget 2027", 1)]);
        assert_eq!(v[0].detail, "Varia\t(10 min)");
    }

    #[test]
    fn renumerotation_apres_modification() {
        let mut v = points("1. A\n  1. A1\n2. B");
        v.remove(0);
        renumeroter(&mut v);
        assert_eq!(resume(&v), vec![p("1", "A1", 1), p("2", "B", 1)]);
    }

    #[test]
    fn nettoyage() {
        assert_eq!(tidy("a  \n\n\n\nb\n"), "a\n\nb");
    }
}
