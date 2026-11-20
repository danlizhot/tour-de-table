//! Placement des sujets d'une rencontre sous les points de l'ordre du jour, avant la rédaction du
//! compte rendu (voir `summary::par_point_complet`).
//!
//! Une seule réponse qui range tous les sujets sous l'ordre du jour est trop difficile pour un
//! modèle de 9B : il range par thème, se trompe de point (un sujet du point 8 rédigé au point 4,
//! puis répété au point 8), ou numérote les sujets comme un compteur (« 1.4 », « 1.5 »…). Il date
//! bien les sujets, en revanche, d'après l'heure des paragraphes de la transcription
//! (« [16:16] »). D'où, en trois étapes :
//!
//! 1. **Liste des sujets** dans l'ordre de la rencontre, chacun avec son heure, sans numéro de point.
//! 2. **Une question courte par sujet** : sous quel point le rédiger ? La consigne et la liste des points
//!    ([`entete_questions`]) sont gardées avec le préfixe : relues à chaque question, elles en faisaient l'essentiel du
//!    coût (une demi-minute pour une rencontre de deux heures, plus d'une heure pour ses 98 sujets). Poser les sujets
//!    par lots était plus rapide mais moins juste (essai du 9 novembre 2026 : 62 % au lieu de 73 %).
//! 3. **Contrôle de l'ordre** : les points se suivent dans l'ordre de la rencontre. Un sujet hors de
//!    la plus longue suite croissante reçoit une deuxième question, avec les points entre lesquels
//!    il est discuté.
//!
//! Un point resté sans sujet reçoit ensuite une question ciblée plutôt qu'une recherche dans toute
//! la rencontre, qui reprenait des sujets déjà rédigés ailleurs ou inventait une date.
//!
//! (Essais du 7 novembre 2026 : demander l'heure où commence chaque point, ou de recopier les
//! premiers mots de son passage, a échoué ; le modèle recopie les titres de l'ordre du jour ou
//! répond [00:00] partout.)
//!
//! Le préfixe (ordre du jour et transcription) est gardé par le programme de rédaction : chaque
//! question ne coûte que la lecture de sa consigne et quelques jetons de réponse.

/// Liste des sujets : une ligne courte par sujet.
pub const JETONS_LISTE: usize = 3000;
/// Question sur un sujet : un numéro.
pub const JETONS_QUESTION: usize = 12;
/// Point sans sujet : quelques lignes.
pub const JETONS_POINT_VIDE: usize = 300;
/// Deuxièmes questions au plus, après le contrôle de l'ordre.
pub const MAX_REPRISES: usize = 20;

pub const LISTE_SUJETS: &str = "Tu es secrétaire de réunion. Ci-dessus : l'ordre du jour et la transcription complète \
de la rencontre. Avant la rédaction du compte rendu, fais la liste des sujets réellement discutés, dans l'ordre de la \
rencontre.\n\
Un sujet, c'est une seule chose : une décision, un montant ou un dossier, une explication, une annonce, une échéance. \
Ne regroupe pas tout un point en une ligne, et n'écris jamais deux fois la même chose. Seulement ce qui est dit dans la \
transcription : le contenu de l'ordre du jour (tableaux, listes de tâches) n'est pas un sujet s'il n'est pas discuté. \
Ignore les conversations d'avant le début et d'après la fin de la rencontre.\n\
Écris une ligne par sujet, sous la forme « 23:10 | sujet en quelques mots, avec ses chiffres et ses noms » : l'heure \
entre crochets du paragraphe de la transcription où on en parle, puis le sujet. Rien d'autre : pas de numéro de point, \
pas de titre, pas d'introduction.\n";

/// Début commun des questions, gardé avec le préfixe.
const ENTETE_QUESTION: &str = "Tu es secrétaire de réunion. Ci-dessus : l'ordre du jour et la transcription complète de \
la rencontre. Chaque sujet du compte rendu n'est rédigé que sous un seul point de l'ordre du jour.\n\
Points (numéro, puis titre) :\n{points}\n";

const QUESTION: &str = "Sujet ({heure}) : {sujet}\n\
{passage}\
Sous quel point ce sujet doit-il être rédigé ? Celui pendant lequel il est discuté, ou un autre point si son titre \
nomme clairement ce sujet. Un point d'adoption (ordre du jour, compte rendu) ne reçoit que ce qui est dit de cette \
adoption ou du document adopté. Réponds seulement par le numéro du point.\n";

const POINT_VIDE: &str = "Tu es secrétaire de réunion. Ci-dessus : l'ordre du jour et la transcription complète de la \
rencontre. Le point « {point} » de l'ordre du jour a-t-il été discuté ?\n\
{autres}\
Si oui, écris ses sujets, une ligne par sujet : « - sujet en quelques mots, avec ses chiffres et ses noms ». \
N'invente rien : une date ou un chiffre qui n'est pas dit n'existe pas. Si ce point n'a pas été discuté, écris \
seulement « Aucun ».\n";

/// Un sujet de la rencontre : texte, heure où on en parle, point retenu (indice dans l'ordre du jour).
#[derive(Clone, Debug, PartialEq)]
pub struct Sujet {
    pub texte: String,
    pub debut_ms: Option<u64>,
    pub point: Option<usize>,
}

/// Heure « 16:16 », « [16:16] » ou « 1:02:03 » au début de `texte`, en ms, et le reste du texte.
pub fn heure(texte: &str) -> Option<(u64, &str)> {
    let t = texte.trim_start().trim_start_matches('[');
    let fin = t.find(|c: char| !(c.is_ascii_digit() || c == ':')).unwrap_or(t.len());
    let parties: Vec<&str> = t[..fin].split(':').collect();
    if !(2..=3).contains(&parties.len()) || parties.iter().any(|p| p.is_empty() || p.len() > 2) {
        return None;
    }
    let s = parties.iter().try_fold(0u64, |acc, p| p.parse::<u64>().ok().map(|v| acc * 60 + v))?;
    Some((s * 1000, t[fin..].trim_start_matches(']')))
}

/// Durée visée d'une tranche de la liste des sujets, en minutes.
const MINUTES_TRANCHE: u64 = 40;

/// Tranche d'une longue rencontre pour la liste des sujets : heures du premier et du dernier paragraphe (ms) et
/// consigne ajoutée à [`LISTE_SUJETS`].
#[derive(Clone, Debug, PartialEq)]
pub struct Tranche {
    pub debut_ms: u64,
    pub fin_ms: u64,
    pub consigne: String,
}

/// Rencontre d'une heure ou plus : la liste des sujets est demandée par tranches d'au plus [`MINUTES_TRANCHE`] minutes,
/// chacune avec sa limite de jetons. D'un seul coup, le modèle écrit une ligne par paragraphe (98 sujets pour deux
/// heures) et la liste peut être coupée à [`JETONS_LISTE`] : la fin de la rencontre reste alors sans sujet (essai du
/// 15 novembre 2026 ; lui demander un nombre de sujets a empiré la liste). Vide pour une rencontre de moins d'une heure.
pub fn tranches(transcription: &[String]) -> Vec<Tranche> {
    // Paragraphes datés : heure et texte entre crochets, tel qu'écrit dans la transcription.
    let dates: Vec<(u64, &str)> = transcription
        .iter()
        .filter_map(|l| {
            let l = l.trim_start();
            let dans = l.strip_prefix('[')?.split_once(']')?.0;
            heure(l).map(|(ms, _)| (ms, dans))
        })
        .collect();
    let fin = dates.iter().map(|d| d.0).max().unwrap_or(0);
    let minutes = fin / 60_000;
    if minutes < 60 {
        return Vec::new();
    }
    let k = minutes.div_ceil(MINUTES_TRANCHE);
    (0..k)
        .filter_map(|j| {
            let (a, b) = (fin * j / k, fin * (j + 1) / k);
            let dedans: Vec<&(u64, &str)> = dates.iter().filter(|d| d.0 >= a && (d.0 < b || (j + 1 == k && d.0 <= b))).collect();
            let (premier, dernier) = (dedans.first()?, dedans.last()?);
            Some(Tranche {
                debut_ms: premier.0,
                fin_ms: dernier.0,
                consigne: format!(
                    "Cette rencontre est longue : la liste se fait en plusieurs parties. Ne fais ici que la partie qui va du \
                     paragraphe [{}] au paragraphe [{}] inclus : seulement les sujets discutés dans ces paragraphes, avec leur \
                     heure. Les sujets d'avant et d'après sont listés à part.\n",
                    premier.1, dernier.1
                ),
            })
        })
        .collect()
}

/// Lignes de `reponse` dont l'heure est dans la tranche ; une ligne sans heure est gardée.
pub fn lignes_de_la_tranche(reponse: &str, t: &Tranche) -> String {
    reponse
        .lines()
        .filter(|l| {
            let l = l.trim().trim_start_matches(['-', '*', '•', ' ']);
            heure(l).is_none_or(|(ms, _)| ms >= t.debut_ms && ms <= t.fin_ms)
        })
        .map(|l| format!("{l}\n"))
        .collect()
}

/// Sujets d'après la réponse à [`LISTE_SUJETS`] (lignes « 23:10 | sujet »), sans doublon.
pub fn sujets(reponse: &str) -> Vec<Sujet> {
    let mut v: Vec<Sujet> = Vec::new();
    for ligne in reponse.lines() {
        let ligne = ligne.trim().trim_start_matches(['-', '*', '•', ' ']).replace("**", "");
        let (debut_ms, texte) = match heure(&ligne) {
            Some((ms, reste)) => (Some(ms), reste.trim_start().trim_start_matches(['|', ':', '-', '–']).trim().to_string()),
            // Sans heure : « 1.4 | sujet » ou une introduction du modèle (sans séparateur).
            None => match ligne.split_once('|') {
                Some((_, t)) => (None, t.trim().to_string()),
                None => continue,
            },
        };
        // « [23:10 | sujet] » : le crochet ouvrant est retiré avec l'heure, le fermant reste au bout.
        let texte = match texte.strip_suffix(']') {
            Some(t) if ligne.starts_with('[') && !t.contains('[') => t.trim_end().to_string(),
            _ => texte,
        };
        if texte.is_empty() || v.iter().any(|s| s.texte == texte) {
            continue;
        }
        v.push(Sujet { texte, debut_ms, point: None });
    }
    v
}

/// Liste « 7. titre » des points `choix`.
fn liste_points(titres: &[&str], choix: &[usize]) -> String {
    choix.iter().map(|&i| format!("{}\n", titres[i])).collect()
}

/// Début des questions (consigne et points `choix`), à la suite du préfixe et gardé avec lui.
pub fn entete_questions(titres: &[&str], choix: &[usize]) -> String {
    ENTETE_QUESTION.replace("{points}", &liste_points(titres, choix))
}

/// Question sur un sujet (à la suite de [`entete_questions`]). `entre` : points entre lesquels il est discuté,
/// d'après les sujets voisins, pour une deuxième question.
pub fn question(s: &Sujet, entre: Option<(usize, usize)>, titres: &[&str]) -> String {
    let heure = s.debut_ms.map_or_else(|| "heure inconnue".to_string(), |ms| format!("vers [{}]", crate::storage::clock(ms)));
    let passage = match entre {
        Some((a, b)) if a == b => format!("Les sujets discutés juste avant et juste après relèvent du point « {} ».\n", titres[a]),
        Some((a, b)) => format!("Il est discuté après le point « {} » et avant le point « {} ».\n", titres[a], titres[b]),
        None => String::new(),
    };
    QUESTION
        .replace("{heure}", &heure)
        .replace("{sujet}", &s.texte)
        .replace("{passage}", &passage)
}

/// Point choisi dans la réponse à une question : le premier numéro d'un point de `choix`.
pub fn point_choisi(reponse: &str, numeros: &[&str], choix: &[usize]) -> Option<usize> {
    reponse
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|n| n.trim_end_matches('.'))
        .filter(|n| !n.is_empty())
        .find_map(|n| choix.iter().copied().find(|&i| numeros[i] == n))
}

/// Sujets hors de l'ordre de la rencontre : ceux qui ne font pas partie de la plus longue suite de
/// points non décroissante (dans l'ordre de l'heure), avec les points des sujets de la suite qui
/// les entourent. Les sujets sans heure ou sans point ne sont pas contrôlés.
pub fn hors_suite(sujets: &[Sujet]) -> Vec<(usize, (usize, usize))> {
    let mut dates: Vec<(u64, usize, usize)> =
        sujets.iter().enumerate().filter_map(|(k, s)| Some((s.debut_ms?, s.point?, k))).collect();
    dates.sort_by_key(|&(t, _, k)| (t, k));
    // Quelques dizaines de sujets : O(n²) suffit.
    let n = dates.len();
    let mut long = vec![1usize; n];
    let mut prec = vec![None; n];
    for i in 0..n {
        for j in 0..i {
            if dates[j].1 <= dates[i].1 && long[j] + 1 > long[i] {
                long[i] = long[j] + 1;
                prec[i] = Some(j);
            }
        }
    }
    let mut dans = vec![false; n];
    let mut courant = (0..n).max_by_key(|&i| (long[i], std::cmp::Reverse(i)));
    while let Some(i) = courant {
        dans[i] = true;
        courant = prec[i];
    }
    (0..n)
        .filter(|&i| !dans[i])
        .filter_map(|i| {
            let avant = (0..i).rev().find(|&j| dans[j]).map(|j| dates[j].1);
            let apres = (i + 1..n).find(|&j| dans[j]).map(|j| dates[j].1);
            let entre = match (avant, apres) {
                (Some(a), Some(b)) => (a, b),
                (Some(a), None) => (a, a),
                (None, Some(b)) => (b, b),
                (None, None) => return None,
            };
            Some((dates[i].2, entre))
        })
        .collect()
}

/// Consigne d'un point sans sujet : les sujets déjà rangés ailleurs, à ne pas reprendre.
pub fn consigne_point_vide(titre: &str, autres: &[(&str, Vec<String>)]) -> String {
    let mut liste = String::new();
    for (t, v) in autres.iter().filter(|(_, v)| !v.is_empty()) {
        liste.push_str(&format!("« {t} » :\n"));
        for s in v {
            liste.push_str(&format!("- {s}\n"));
        }
    }
    let autres = if liste.is_empty() {
        String::new()
    } else {
        format!("Ces sujets sont déjà rédigés sous d'autres points : ne les reprends pas.\n{liste}")
    };
    POINT_VIDE.replace("{point}", titre).replace("{autres}", &autres)
}

/// Sujets d'un point sans sujet d'après la réponse à [`POINT_VIDE`] ; vide pour « Aucun ».
pub fn sujets_du_point_vide(reponse: &str) -> Vec<String> {
    reponse
        .lines()
        .filter_map(|l| l.trim().strip_prefix(['-', '*', '•']))
        .map(|s| s.trim().replace("**", ""))
        .filter(|s| !s.is_empty() && !s.to_lowercase().starts_with("aucun"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tranches_seulement_apres_une_heure() {
        let p = |h: &str| format!("[{h}] Texte.");
        assert!(tranches(&[p("00:00"), p("59:30")]).is_empty());
        let longue: Vec<String> = ["00:00", "20:00", "39:00", "41:00", "1:10:00", "1:25:00", "2:01:21"].iter().map(|h| p(h)).collect();
        let t = tranches(&longue);
        // 121 min : quatre tranches d'environ 30 min ; la troisième (1:00:40 à 1:31:00) commence à 1:10:00.
        assert_eq!(t.iter().map(|x| (x.debut_ms / 1000, x.fin_ms / 1000)).collect::<Vec<_>>(), vec![(0, 1200), (2340, 2460), (4200, 5100), (7281, 7281)]);
        assert!(t[2].consigne.contains("du paragraphe [1:10:00] au paragraphe [1:25:00] inclus"), "{}", t[2].consigne);
        assert_eq!(lignes_de_la_tranche("1:05:00 | avant\n- 1:12:00 | dedans\nsans heure\n1:30:00 | après", &t[2]), "- 1:12:00 | dedans\nsans heure\n");
    }

    #[test]
    fn heures_et_sujets() {
        assert_eq!(heure("[16:16] Salle"), Some((976_000, " Salle")));
        assert_eq!(heure("1:02:03"), Some((3_723_000, "")));
        assert_eq!(heure("2026-2027"), None);
        assert_eq!(heure("7.1"), None);
        let v = sujets(
            "Voici les sujets :\n- 02:15 | Logiciel pour le compte rendu\n[16:57] | Coupure de 12 500 $\n\
16:57 | Coupure de 12 500 $\n1.4 | Pont 00248",
        );
        let textes: Vec<&str> = v.iter().map(|s| s.texte.as_str()).collect();
        assert_eq!(textes, ["Logiciel pour le compte rendu", "Coupure de 12 500 $", "Pont 00248"]);
        assert_eq!(v[1].debut_ms, Some(1_017_000));
        assert_eq!(v[2].debut_ms, None);
        // Ligne entière entre crochets (Qwen3.6, 20 novembre 2026) ; un crochet du sujet lui-même reste.
        let v = sujets("[03:29 | Adoption de l'ordre du jour]\n[04:30] Plan [annexe]");
        let textes: Vec<&str> = v.iter().map(|s| s.texte.as_str()).collect();
        assert_eq!(textes, ["Adoption de l'ordre du jour", "Plan [annexe]"]);
    }

    #[test]
    fn ordre_de_la_rencontre() {
        let s = |t: u64, point| Sujet { texte: String::new(), debut_ms: Some(t * 1000), point: Some(point) };
        // Points 3, 4, 4, 5, [10 : hors suite], 7, 8, 8, 10 (en fin, normal).
        let v = [s(10, 3), s(20, 4), s(30, 4), s(40, 5), s(50, 10), s(60, 7), s(70, 8), s(80, 8), s(90, 10)];
        assert_eq!(hors_suite(&v), [(4, (5, 7))]);
        // Sujet hors suite en tête : seulement le point qui suit.
        assert_eq!(hors_suite(&[s(5, 9), s(10, 2), s(20, 3)]), [(0, (2, 2))]);
        // Sans heure : pas contrôlé.
        let mut w = v.to_vec();
        w[4].debut_ms = None;
        assert!(hors_suite(&w).is_empty());
    }

    #[test]
    fn questions_et_reponses() {
        let titres = ["5. Liste des chèques", "6. Suivi tâches", "7. Coupure", "11. Prochaine rencontre"];
        let sujet = Sujet { texte: "États financiers".into(), debut_ms: Some(740_000), point: Some(3) };
        let q = question(&sujet, Some((0, 2)), &titres);
        assert!(
            q.contains("Sujet (vers [12:20]) : États financiers")
                && q.contains("après le point « 5. Liste des chèques » et avant le point « 7. Coupure »"),
            "{q}"
        );
        let numeros = ["5", "6", "7", "11"];
        let e = entete_questions(&titres, &[0, 2]);
        assert!(e.contains("Points (numéro, puis titre) :\n5. Liste des chèques\n7. Coupure\n") && !e.contains("6. Suivi"), "{e}");
        assert_eq!(point_choisi("6", &numeros, &[0, 1, 2, 3]), Some(1));
        assert_eq!(point_choisi("Le point 7. Coupure", &numeros, &[0, 1, 2, 3]), Some(2));
        assert_eq!(point_choisi("Point 11", &numeros, &[0, 1]), None);
        assert_eq!(sujets_du_point_vide("Aucun"), Vec::<String>::new());
        assert_eq!(sujets_du_point_vide("- Lettre à la ministre\n- **Coupure** de 12 500 $"), ["Lettre à la ministre", "Coupure de 12 500 $"]);
        let c = consigne_point_vide("11. Prochaine rencontre", &[("4. Rapport", vec!["Factures".into()]), ("7. Coupure", vec![])]);
        assert!(c.contains("« 4. Rapport » :\n- Factures") && !c.contains("7. Coupure"), "{c}");
    }
}
