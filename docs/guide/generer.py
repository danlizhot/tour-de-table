"""Produit docs/guide/guide-utilisation.pdf à partir de guide.html, style.css, couverture.html et des captures.

Usage : python3 docs/guide/generer.py [version]
Requiert Chromium ou Chrome (impression PDF sans en-tête du navigateur), Pillow et les outils
poppler (pdftotext, pdfunite). La version par défaut est celle de Cargo.toml.

Les captures (docs/guide/captures/, 1440 × 900, thème clair) montrent une rencontre entièrement
fictive. Dans guide.html, chaque <figure data-img=…> peut porter :
  data-crop="x0:y0:x1:y1"    recadrage, en pixels de la capture ;
  data-bulles="n:x:y …"      bulles numérotées, aux mêmes coordonnées.
"""
import datetime, html, os, re, shutil, subprocess, sys
from PIL import Image

ICI = os.path.dirname(os.path.abspath(__file__))
RACINE = os.path.dirname(os.path.dirname(ICI))
BUILD = os.path.join(ICI, "build")
CAPTURES = os.path.join(ICI, "captures")
REF_LARGEUR = 1440

MOIS = ["janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre"]
NAVIGATEURS = ["chromium", "chromium-browser", "google-chrome", "google-chrome-stable", "brave", "msedge"]


def version():
    if len(sys.argv) > 1:
        return sys.argv[1]
    m = re.search(r'^version\s*=\s*"([^"]+)"', open(os.path.join(RACINE, "Cargo.toml"), encoding="utf-8").read(), re.M)
    return m.group(1) if m else "?"


def navigateur():
    for n in NAVIGATEURS:
        if chemin := shutil.which(n):
            return chemin
    sys.exit("Chromium ou Chrome introuvable : il sert à imprimer le guide en PDF.")


def figures(source):
    """Remplace chaque <figure data-img=…> par l'image recadrée et ses bulles numérotées."""
    os.makedirs(os.path.join(BUILD, "img"), exist_ok=True)
    n = 0

    def remplacer(m):
        nonlocal n
        n += 1
        attrs = dict(re.findall(r'(data-[a-z]+)="([^"]*)"', m.group(1)))
        classes = re.search(r'class="([^"]*)"', m.group(1)).group(1)
        im = Image.open(os.path.join(CAPTURES, attrs["data-img"])).convert("RGB")
        k = im.width / REF_LARGEUR
        x0, y0, x1, y1 = 0, 0, REF_LARGEUR, im.height / k
        if "data-crop" in attrs:
            x0, y0, x1, y1 = map(float, attrs["data-crop"].split(":"))
        im = im.crop((int(x0 * k), int(y0 * k), int(x1 * k), int(y1 * k)))
        nom = f"fig{n:02d}.png"
        im.save(os.path.join(BUILD, "img", nom), optimize=True)
        bulles = ""
        for b in attrs.get("data-bulles", "").split():
            num, bx, by = b.split(":")
            px = (float(bx) - x0) / (x1 - x0) * 100
            py = (float(by) - y0) / (y1 - y0) * 100
            bulles += f'<span class="bulle" style="left:{px:.2f}%;top:{py:.2f}%">{num}</span>'
        return f'<figure class="{classes}"><div class="cadre"><img src="img/{nom}" alt="">{bulles}</div>{m.group(2)}</figure>'

    return re.sub(r"<figure([^>]*)>(.*?)</figure>", remplacer, source, flags=re.S)


def chapitres(source):
    return [(i, html.unescape(re.sub("<[^>]+>", "", t)).strip())
            for i, t in re.findall(r'<h1 id="([^"]+)">(.*?)</h1>', source, re.S)]


def construire(source, pages):
    chap = chapitres(source)
    toc = "".join(
        f'<li><span class="num">{k}</span><span class="titre"><a href="#{i}">{html.escape(t)}</a></span>'
        f'<span class="page">{pages.get(i, "")}</span></li>'
        for k, (i, t) in enumerate(chap, 1))
    s = source.replace("<!--TOC-->", toc)
    s = re.sub(r'<a class="ref" href="#([^"]+)"',
               lambda m: f'<a class="ref" href="#{m.group(1)}"' + (f' data-page="{pages[m.group(1)]}"' if m.group(1) in pages else ""), s)
    with open(os.path.join(BUILD, "guide.html"), "w", encoding="utf-8") as f:
        f.write(s)


def rendre(entree, sortie):
    commande = [navigateur(), "--headless", "--disable-gpu", "--no-pdf-header-footer", "--run-all-compositor-stages-before-draw",
                "--virtual-time-budget=5000", f"--print-to-pdf={sortie}", "file://" + entree]
    if subprocess.run(commande, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0:
        return
    # Chromium en snap refuse de démarrer depuis un processus aux droits restreints (terminal intégré à une
    # application) : on le lance alors par le gestionnaire de services de la session.
    if not shutil.which("systemd-run"):
        sys.exit("L'impression PDF par Chromium a échoué.")
    subprocess.run(["systemd-run", "--user", "--quiet", "--wait", "--pipe", "--collect", *commande],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def compacte(texte):
    return re.sub(r"[\W_]+", "", texte).lower()


def trouver_pages(pdf, source):
    """Page (numérotée à partir de 1) où commence chaque chapitre : son titre est en haut de la page."""
    chap = chapitres(source)
    texte = subprocess.run(["pdftotext", "-layout", pdf, "-"], check=True, capture_output=True, text=True).stdout
    res, suivant = {}, 0
    for no, page in enumerate(texte.split("\f"), 1):
        haut = compacte(" ".join([l for l in page.splitlines() if l.strip()][1:4]))
        if suivant < len(chap) and compacte(chap[suivant][1]) in haut:
            res[chap[suivant][0]] = no
            suivant += 1
    return res


def main():
    shutil.rmtree(BUILD, ignore_errors=True)
    os.makedirs(BUILD)
    shutil.copy(os.path.join(ICI, "style.css"), BUILD)
    source = figures(open(os.path.join(ICI, "guide.html"), encoding="utf-8").read())
    corps = os.path.join(BUILD, "corps.pdf")
    pages = {}
    for _ in range(3):  # la table des matières reçoit les vrais numéros de page
        construire(source, pages)
        rendre(os.path.join(BUILD, "guide.html"), corps)
        nouvelles = trouver_pages(corps, source)
        if nouvelles == pages:
            break
        pages = nouvelles
    manquants = [t for i, t in chapitres(source) if i not in pages]
    if manquants:
        sys.exit(f"Chapitres introuvables dans le PDF : {manquants}")

    d = datetime.date.today()
    couv = open(os.path.join(ICI, "couverture.html"), encoding="utf-8").read()
    couv = couv.replace("{{VERSION}}", version()).replace("{{DATE}}", f"{MOIS[d.month - 1].capitalize()} {d.year}")
    with open(os.path.join(BUILD, "couverture.html"), "w", encoding="utf-8") as f:
        f.write(couv)
    rendre(os.path.join(BUILD, "couverture.html"), os.path.join(BUILD, "couverture.pdf"))

    sortie = os.path.join(ICI, "guide-utilisation.pdf")
    subprocess.run(["pdfunite", os.path.join(BUILD, "couverture.pdf"), corps, sortie], check=True)
    n = len(subprocess.run(["pdftotext", sortie, "-"], check=True, capture_output=True, text=True).stdout.split("\f")) - 1
    print(f"{sortie} : {n} pages, version {version()}")


if __name__ == "__main__":
    main()
