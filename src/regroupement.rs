//! Regroupement des empreintes de voix : qui est la même personne.
//!
//! Le regroupement de sherpa-onnx, à seuil fixe, basculait pour des écarts inaudibles (un arrondi
//! de l'audio, ±10 % de volume : de 67 à 91 % d'accord avec le résultat vérifié à l'écoute). Ici :
//! regroupement hiérarchique à liaison moyenne sur la similarité cosinus de morceaux de quelques
//! secondes, arrêté sous une similarité moyenne fixe. Le choix du nombre de voix par la silhouette a
//! été essayé : il réunissait des personnes dans les longues rencontres (6 voix pour 8 personnes).

/// Empreinte normalisée et poids (durée en ms) d'un morceau de parole.
pub struct Morceau {
    pub empreinte: Vec<f32>,
    pub poids: f32,
}

pub fn normaliser(v: &mut [f32]) {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
}

fn similarites(m: &[Morceau]) -> Vec<Vec<f32>> {
    let n = m.len();
    let mut s = vec![vec![0.0f32; n]; n];
    for i in 0..n {
        for j in i..n {
            let v: f32 = m[i].empreinte.iter().zip(&m[j].empreinte).map(|(a, b)| a * b).sum();
            s[i][j] = v;
            s[j][i] = v;
        }
    }
    s
}

/// Fusions successives du regroupement hiérarchique (liaison moyenne pondérée par la durée) :
/// (groupe gardé, groupe absorbé, similarité moyenne au moment de la fusion).
fn fusions(sim: &[Vec<f32>], poids: &[f32]) -> Vec<(usize, usize, f32)> {
    let n = sim.len();
    // Similarité moyenne entre groupes (somme des similarités pondérées / somme des poids croisés).
    let mut s: Vec<Vec<f32>> = sim.to_vec();
    let mut p: Vec<f32> = poids.to_vec();
    let mut vivant = vec![true; n];
    let mut out = Vec::with_capacity(n.saturating_sub(1));
    for _ in 1..n {
        let mut meilleur = (0, 0, f32::NEG_INFINITY);
        for i in 0..n {
            if !vivant[i] {
                continue;
            }
            for j in (i + 1)..n {
                if vivant[j] && s[i][j] > meilleur.2 {
                    meilleur = (i, j, s[i][j]);
                }
            }
        }
        let (a, b, v) = meilleur;
        // Lance-Williams, liaison moyenne : moyenne des deux, pondérée par les poids des groupes.
        let (pa, pb) = (p[a], p[b]);
        for k in 0..n {
            if vivant[k] && k != a && k != b {
                let nv = (pa * s[a][k] + pb * s[b][k]) / (pa + pb);
                s[a][k] = nv;
                s[k][a] = nv;
            }
        }
        p[a] = pa + pb;
        vivant[b] = false;
        out.push((a, b, v));
    }
    out
}

/// Étiquettes (0..k) après les `n - k` premières fusions.
fn etiquettes(n: usize, fusions: &[(usize, usize, f32)], k: usize) -> Vec<usize> {
    let mut parent: Vec<usize> = (0..n).collect();
    for &(a, b, _) in &fusions[..n.saturating_sub(k).min(fusions.len())] {
        parent[b] = a;
    }
    let racine = |mut i: usize| {
        while parent[i] != i {
            i = parent[i];
        }
        i
    };
    let mut numeros = std::collections::HashMap::new();
    (0..n)
        .map(|i| {
            let r = racine(i);
            let l = numeros.len();
            *numeros.entry(r).or_insert(l)
        })
        .collect()
}

/// Numéro de voix de chaque morceau : les fusions s'arrêtent quand la similarité moyenne entre les
/// deux groupes les plus proches passe sous `seuil`.
pub fn regrouper(morceaux: &[Morceau], seuil: f32) -> Vec<usize> {
    let n = morceaux.len();
    if n < 2 {
        return vec![0; n];
    }
    let sim = similarites(morceaux);
    let poids: Vec<f32> = morceaux.iter().map(|m| m.poids).collect();
    let f = fusions(&sim, &poids);
    // Les similarités de fusion ne décroissent pas toujours (liaison moyenne) : on s'arrête à la première sous le seuil.
    let k = n - f.iter().take_while(|(_, _, v)| *v >= seuil).count();
    etiquettes(n, &f, k)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn morceau(v: &[f32]) -> Morceau {
        let mut e = v.to_vec();
        normaliser(&mut e);
        Morceau { empreinte: e, poids: 1000.0 }
    }

    /// Trois voix nettement séparées, avec un peu de bruit : trois groupes.
    #[test]
    fn trois_voix() {
        let mut m = Vec::new();
        for i in 0..30 {
            let b = (i as f32 * 0.37).sin() * 0.15;
            m.push(morceau(&[1.0, b, 0.0, 0.1]));
            m.push(morceau(&[b, 1.0, 0.1, 0.0]));
            m.push(morceau(&[0.0, 0.1, 1.0 + b, b]));
        }
        let e = regrouper(&m, 0.5);
        assert_eq!(*e.iter().max().unwrap(), 2);
        for i in 0..30 {
            assert_eq!(e[3 * i], e[0]);
            assert_eq!(e[3 * i + 1], e[1]);
            assert_eq!(e[3 * i + 2], e[2]);
        }
        // Seuil trop bas : tout le monde réuni.
        assert!(regrouper(&m, -1.0).iter().all(|&x| x == 0));
    }
}
