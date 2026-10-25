//! Spécialisation français québécois.
//!
//! Whisper n'a pas de code de langue « fr-CA » : on oriente le décodage avec
//! une invite (prompt) écrite en québécois et dans la typographie de l'OQLF,
//! puis on normalise la sortie (heures, montants, ponctuation) et on applique
//! le lexique de l'utilisateur.

/// Invite de base. Whisper imite le registre, le vocabulaire et la typographie
/// du texte qui précède : on y met des québécismes courants en réunion,
/// « 14 h 30 », « 2 500 $ » et la ponctuation sans espace avant ? et !.
pub const BASE_PROMPT: &str = "Réunion au Québec, en français québécois. \
Bon ben, on commence-tu? Tsé, c'est correct, on va checker ça pis on se r'parle tantôt. \
Y'a pas de trouble, c'est pas pire pantoute. Je t'envoie un courriel avant le dîner; \
la rencontre est à 14 h 30 et le budget est de 2 500 $. Fin de semaine, cellulaire, \
présentement, magasiner, souper, ça a pas d'allure, c'est plate, j'suis tanné.";

/// Phrases que Whisper « hallucine » en français sur du silence ou de la musique
/// (héritées des sous-titres de ses données d'entraînement).
const HALLUCINATIONS: &[&str] = &[
    "sous-titrage société radio-canada",
    "sous-titrage st' 501",
    "sous-titrage st'501",
    "sous-titres réalisés par la communauté d'amara.org",
    "sous-titres réalisés para la communauté d'amara.org",
    "sous-titres par la communauté d'amara.org",
    "merci d'avoir regardé cette vidéo",
    "merci d'avoir regardé",
    "abonnez-vous à la chaîne",
    "n'oubliez pas de vous abonner",
    "sous-titrage fr",
];

pub struct Glossary {
    terms: Vec<String>,
    replacements: Vec<(String, String)>,
}

impl Glossary {
    /// Une entrée par ligne : `terme` (ajouté à l'invite) ou `mauvais => bon`.
    pub fn parse(text: &str) -> Self {
        let mut terms = Vec::new();
        let mut replacements = Vec::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            if let Some((from, to)) = line.split_once("=>") {
                let (from, to) = (from.trim(), to.trim());
                if !from.is_empty() {
                    replacements.push((from.to_string(), to.to_string()));
                    terms.push(to.to_string());
                }
            } else {
                terms.push(line.to_string());
            }
        }
        Self { terms, replacements }
    }

    /// Construit l'invite : base + lexique + fin du texte déjà transcrit
    /// (pour la cohérence des noms et du contexte d'un énoncé à l'autre).
    pub fn prompt(&self, previous: &str, agenda_hint: &str) -> String {
        let mut p = String::from(BASE_PROMPT);
        if !agenda_hint.is_empty() {
            p.push_str(" Ordre du jour : ");
            p.push_str(agenda_hint);
            p.push('.');
        }
        if !self.terms.is_empty() {
            p.push_str(" Termes : ");
            p.push_str(&self.terms.join(", "));
            p.push('.');
        }
        // Whisper garde au plus ~224 jetons d'invite (la fin) : on limite le contexte.
        let tail: String = {
            let chars: Vec<char> = previous.chars().collect();
            chars[chars.len().saturating_sub(200)..].iter().collect()
        };
        if !tail.is_empty() {
            p.push(' ');
            p.push_str(tail.trim());
        }
        p
    }

    /// Termes du lexique, tels qu'ils doivent être écrits.
    pub fn termes(&self) -> &[String] {
        &self.terms
    }

    pub fn apply(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (from, to) in &self.replacements {
            out = replace_word(&out, from, to);
        }
        out
    }
}

/// Remplace `from` seulement s'il forme un mot entier (insensible à la casse).
fn replace_word(text: &str, from: &str, to: &str) -> String {
    let lower = text.to_lowercase();
    let needle = from.to_lowercase();
    if lower.len() != text.len() {
        // Changement de longueur en minuscules (rare) : remplacement simple.
        return text.replace(from, to);
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (i, _) in lower.match_indices(&needle) {
        let before = text[..i].chars().next_back();
        let after = text[i + needle.len()..].chars().next();
        let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
        if i >= last && boundary(before) && boundary(after) {
            out.push_str(&text[last..i]);
            out.push_str(to);
            last = i + needle.len();
        }
    }
    out.push_str(&text[last..]);
    out
}

pub fn is_hallucination(text: &str) -> bool {
    let t = text.trim().trim_matches(|c: char| c == '.' || c == '!' || c.is_whitespace());
    let t = t.to_lowercase();
    t.is_empty()
        || HALLUCINATIONS.iter().any(|h| t == *h || t.starts_with(h))
        || t.chars().all(|c| !c.is_alphanumeric())
}

/// Typographie québécoise (OQLF) et formats usuels.
pub fn normalize(text: &str) -> String {
    let s = punctuation(text.trim());
    let s = fix_hours(&s, true);
    fix_dollars(&s).replace("  ", " ")
}

/// Même typographie pour un document Markdown produit par le modèle de langue :
/// indentation et blocs de code préservés, et « 12:30 » laissé tel quel
/// (ce sont des horodatages mm:ss de la transcription, pas des heures).
pub fn normalize_document(md: &str) -> String {
    let mut in_code = false;
    md.lines()
        .map(|line| {
            if line.trim_start().starts_with("```") {
                in_code = !in_code;
            }
            if in_code || line.trim().is_empty() {
                return line.to_string();
            }
            let indent = &line[..line.len() - line.trim_start().len()];
            let body = fix_dollars(&fix_hours(&punctuation(line.trim_start()), false));
            format!("{indent}{body}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Pas d'espace avant ? ! ; (usage québécois), espace avant le deux-points.
fn punctuation(text: &str) -> String {
    let mut s = text.to_string();
    for p in ['?', '!', ';'] {
        for sp in [" ", "\u{a0}", "\u{202f}"] {
            s = s.replace(&format!("{sp}{p}"), &p.to_string());
        }
    }
    fix_colon_spacing(&s)
}

fn fix_colon_spacing(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        // « 10:30 » est une heure ; on ne touche qu'au deux-points de ponctuation.
        let digit_around = i > 0
            && chars[i - 1].is_ascii_digit()
            && chars.get(i + 1).is_some_and(|c| c.is_ascii_digit());
        let url = chars.get(i + 1) == Some(&'/');
        if c == ':' && !digit_around && !url && i > 0 && !chars[i - 1].is_whitespace() {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// « 14h30 », « 14:30 », « 14 h30 » → « 14 h 30 » ; « 9h » → « 9 h ».
fn fix_hours(s: &str, colon_hours: bool) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 8);
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() && (i == 0 || !chars[i - 1].is_ascii_digit()) {
            let mut j = i;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            let hours: String = chars[i..j].iter().collect();
            let mut k = j;
            if k < chars.len() && chars[k] == ' ' {
                k += 1;
            }
            let sep = chars.get(k).copied();
            let is_h = sep == Some('h')
                && chars.get(k + 1).is_none_or(|c| !c.is_alphabetic());
            let is_colon = colon_hours && sep == Some(':') && k == j;
            if hours.len() <= 2 && hours.parse::<u32>().is_ok_and(|h| h < 24) && (is_h || is_colon) {
                let mut m = k + 1;
                if m < chars.len() && chars[m] == ' ' && is_h {
                    m += 1;
                }
                let mut n = m;
                while n < chars.len() && chars[n].is_ascii_digit() {
                    n += 1;
                }
                let minutes: String = chars[m..n].iter().collect();
                let after_ok = chars.get(n).is_none_or(|c| !c.is_alphanumeric());
                if minutes.len() == 2 && after_ok {
                    out.push_str(&format!("{hours} h {minutes}"));
                    i = n;
                    continue;
                }
                if is_h && minutes.is_empty() {
                    out.push_str(&format!("{hours} h"));
                    i = k + 1;
                    continue;
                }
            }
            out.extend(&chars[i..j]);
            i = j;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// « $10 », « 10$ » → « 10 $ » (le symbole suit le montant au Québec).
fn fix_dollars(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 4);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '$' && chars.get(i + 1).is_some_and(|c| c.is_ascii_digit()) {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_ascii_digit() || matches!(chars[j], ',' | '.' | ' ' | '\u{a0}') && chars.get(j + 1).is_some_and(|c| c.is_ascii_digit())) {
                j += 1;
            }
            out.extend(&chars[i + 1..j]);
            out.push_str(" $");
            i = j;
            continue;
        }
        if c == '$' && i > 0 && chars[i - 1].is_ascii_digit() {
            out.push(' ');
        }
        out.push(c);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typographie() {
        assert_eq!(normalize("On commence-tu ?"), "On commence-tu?");
        assert_eq!(normalize("Attention !"), "Attention!");
        assert_eq!(normalize("Ordre du jour: budget"), "Ordre du jour : budget");
        assert_eq!(normalize("On se voit à 10:30."), "On se voit à 10 h 30.");
        assert_eq!(normalize("Voir https://exemple.ca"), "Voir https://exemple.ca");
    }

    #[test]
    fn document_markdown() {
        let md = "# Compte rendu\n**Date**: 2026-10-22\n  - Suivi à 14h30 ?\n[12:30] Budget de $500\n```\ncode ?\n```";
        assert_eq!(
            normalize_document(md),
            "# Compte rendu\n**Date** : 2026-10-22\n  - Suivi à 14 h 30?\n[12:30] Budget de 500 $\n```\ncode ?\n```"
        );
    }

    #[test]
    fn heures() {
        assert_eq!(normalize("la rencontre à 14h30"), "la rencontre à 14 h 30");
        assert_eq!(normalize("vers 9h demain"), "vers 9 h demain");
        assert_eq!(normalize("à 14 h 30"), "à 14 h 30");
        assert_eq!(normalize("à 14 h30"), "à 14 h 30");
        assert_eq!(normalize("le 25h de travail"), "le 25h de travail");
        assert_eq!(normalize("3 heures"), "3 heures");
    }

    #[test]
    fn dollars() {
        assert_eq!(normalize("ça coûte $10 par mois"), "ça coûte 10 $ par mois");
        assert_eq!(normalize("budget de 2 500$"), "budget de 2 500 $");
        assert_eq!(normalize("budget de 2 500 $"), "budget de 2 500 $");
    }

    #[test]
    fn hallucinations() {
        assert!(is_hallucination("Sous-titrage Société Radio-Canada"));
        assert!(is_hallucination("  Merci d'avoir regardé!  "));
        assert!(is_hallucination("..."));
        assert!(!is_hallucination("Radio-Canada en parle ce matin."));
    }

    #[test]
    fn lexique() {
        let g = Glossary::parse("# commentaire\nDesjardins\nhydro québec => Hydro-Québec\n");
        assert_eq!(g.apply("On appelle hydro québec demain."), "On appelle Hydro-Québec demain.");
        assert_eq!(g.apply("hydro québecois"), "hydro québecois");
        let p = g.prompt("Bonjour tout le monde.", "Budget ; Varia");
        assert!(p.contains("Ordre du jour : Budget ; Varia."));
        assert!(p.contains("Desjardins") && p.contains("Hydro-Québec"));
        assert!(p.ends_with("Bonjour tout le monde."));
    }
}
