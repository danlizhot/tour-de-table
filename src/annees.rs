//! Années dites en deux nombres (« en 20-24 », « depuis vingt vingt-quatre ») : récrites « 2024 » par le nettoyage.
//!
//! La règle est étroite exprès : « 20-25 » est aussi une fourchette (« 20-25 hectares », « 20-25 % »), et coller les
//! deux nombres à l'aveugle inventerait une année en perdant une mesure. On ne convertit donc que si un mot de temps
//! précède et qu'aucune unité ne suit. La transcription brute, elle, n'est jamais modifiée.

/// Mots après lesquels « 20-24 » est une année.
const AVANT: &[&str] = &["en", "depuis", "année", "l'année", "exercice", "l'exercice", "saison", "budget", "fin", "début", "jusqu'en", "qu'en", "d'ici"];

/// Mots après lesquels « 20-24 » reste une fourchette ou une mesure.
const UNITES: &[&str] = &[
    "$", "%", "dollars", "piastres", "cents", "sous", "hectares", "hectare", "ha", "heures", "heure", "h", "minutes", "min",
    "secondes", "jours", "semaines", "mois", "ans", "années", "personnes", "membres", "participants", "km", "kilomètres", "mètres",
    "m", "tonnes", "voyages", "projets", "dossiers", "mille", "millions", "milliards", "fois", "degrés", "pieds", "acres", "arbres",
    "plants", "lots", "terrains",
];

/// Dernière année tenue pour plausible : au-delà, « 20-45 » est plus souvent une fourchette.
const DERNIERE: u32 = 39;

fn sans_ponctuation(mot: &str) -> &str {
    mot.trim_matches([',', '.', ';', ':', '!', '?', '(', ')', '«', '»', '"'])
}

fn ponctuation_finale(mot: &str) -> &str {
    &mot[mot.trim_end_matches([',', '.', ';', ':', '!', '?', ')', '»', '"']).len()..]
}

/// Deux chiffres, de 00 à `DERNIERE`.
fn deux_chiffres(s: &str) -> Option<u32> {
    (s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten().filter(|n| *n <= DERNIERE)
}

/// Nombre de 10 à 39 écrit en lettres, en un seul mot à traits d'union (« vingt-quatre », « vingt-et-un »).
fn en_lettres(s: &str) -> Option<u32> {
    const DIZAINE: &[(&str, u32)] = &[
        ("dix", 10), ("onze", 11), ("douze", 12), ("treize", 13), ("quatorze", 14), ("quinze", 15), ("seize", 16), ("dix-sept", 17),
        ("dix-huit", 18), ("dix-neuf", 19), ("vingt", 20), ("trente", 30),
    ];
    const UNITE: &[(&str, u32)] =
        &[("et-un", 1), ("un", 1), ("deux", 2), ("trois", 3), ("quatre", 4), ("cinq", 5), ("six", 6), ("sept", 7), ("huit", 8), ("neuf", 9)];
    if let Some((_, n)) = DIZAINE.iter().find(|(mot, _)| *mot == s) {
        return Some(*n);
    }
    let (dizaine, unite) = ["vingt-", "trente-"].iter().find_map(|d| s.strip_prefix(d).map(|u| (if *d == "vingt-" { 20 } else { 30 }, u)))?;
    UNITE.iter().find(|(mot, _)| *mot == unite).map(|(_, n)| dizaine + n)
}

/// Année dite en deux nombres à partir du mot `i` : (année, nombre de mots occupés).
fn annee_a(mots: &[&str], i: usize) -> Option<(u32, usize)> {
    let mot = sans_ponctuation(mots[i]).to_lowercase();
    // En un mot : « 20-24 », « vingt-vingt-quatre ».
    if let Some(reste) = mot.strip_prefix("20-").or_else(|| mot.strip_prefix("vingt-vingt").map(|r| r.strip_prefix('-').unwrap_or(r))) {
        let suite = if mot.starts_with("vingt") { if reste.is_empty() { Some(20) } else { en_lettres(&format!("vingt-{reste}")) } } else { deux_chiffres(reste) };
        return suite.map(|n| (2000 + n, 1));
    }
    // En deux mots, sans ponctuation entre les deux : « 20 24 », « vingt vingt-quatre ».
    if (mot == "20" || mot == "vingt") && mots[i].len() == sans_ponctuation(mots[i]).len() {
        let suivant = sans_ponctuation(mots.get(i + 1)?).to_lowercase();
        let suite = if mot == "20" { deux_chiffres(&suivant) } else { en_lettres(&suivant) };
        return suite.map(|n| (2000 + n, 2));
    }
    None
}

/// Récrit les années dites en deux nombres, seulement après un mot de temps et hors de toute mesure.
pub fn recrire(texte: &str) -> String {
    let mots: Vec<&str> = texte.split(' ').collect();
    let mut sortie: Vec<String> = Vec::with_capacity(mots.len());
    let mut i = 0;
    while i < mots.len() {
        let avant = i.checked_sub(1).map(|k| sans_ponctuation(mots[k]).to_lowercase().replace('’', "'"));
        let apres_mot_de_temps = avant.as_deref().is_some_and(|a| AVANT.contains(&a))
            // « en 20-25, un autre en 20-24 » : la ponctuation ne sépare pas le mot de temps de l'année.
            && i.checked_sub(1).is_some_and(|k| ponctuation_finale(mots[k]).is_empty());
        if apres_mot_de_temps && let Some((annee, n)) = annee_a(&mots, i) {
            let dernier = mots[i + n - 1];
            let suivant = mots.get(i + n).map(|m| sans_ponctuation(m).to_lowercase());
            let mesure = ponctuation_finale(dernier).is_empty() && suivant.as_deref().is_some_and(|s| UNITES.contains(&s) || s.starts_with('$') || s.starts_with('%'));
            if !mesure {
                sortie.push(format!("{annee}{}", ponctuation_finale(dernier)));
                i += n;
                continue;
            }
        }
        sortie.push(mots[i].to_string());
        i += 1;
    }
    sortie.join(" ")
}

#[cfg(test)]
mod tests {
    use super::recrire;

    #[test]
    fn annees_apres_un_mot_de_temps() {
        assert_eq!(recrire("Il y avait un 53 710 en 20-25, un 35 841 en 20-24, puis un 17 992 en 20-23."), "Il y avait un 53 710 en 2025, un 35 841 en 2024, puis un 17 992 en 2023.");
        assert_eq!(recrire("Depuis 20-24, il n'y avait aucun rapport."), "Depuis 2024, il n'y avait aucun rapport.");
        assert_eq!(recrire("C'est prévu pour l'année 20 26."), "C'est prévu pour l'année 2026.");
        assert_eq!(recrire("On l'a fait en vingt vingt-quatre et en vingt-vingt-cinq."), "On l'a fait en 2024 et en 2025.");
        assert_eq!(recrire("Depuis vingt vingt, c'est comme ça."), "Depuis 2020, c'est comme ça.");
        assert_eq!(recrire("Le budget 20-27 est prêt?"), "Le budget 2027 est prêt?");
    }

    #[test]
    fn fourchettes_et_mesures_gardees() {
        for texte in [
            "On parle de 20-25 hectares par année.",
            "Ça prend 20-25 minutes.",
            "Une hausse de 20-25 %.",
            "En 20-25 minutes, c'est réglé.",
            "Depuis 20 25 ans, rien n'a changé.",
            "Il y en a 20-24 dans la salle.",
            "Entre 20 et 24 personnes.",
            "En 20, 24 dossiers ont été fermés.",
            "Un taux de 20-25 $ l'heure, en 20-25 $ constants.",
            "En 20-45, on verra.",
            "En 2024, tout allait bien.",
            "En vingt minutes, c'est fait.",
        ] {
            assert_eq!(recrire(texte), texte);
        }
    }
}
