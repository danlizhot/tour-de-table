"""Convertit CONFIDENTIALITE.md en page HTML autonome (GitHub Pages).

Usage : python3 -I packaging/politique_html.py CONFIDENTIALITE.md sortie/index.html
"""
import html
import re
import sys


def inline(t):
    t = html.escape(t, quote=False)
    t = re.sub(r"`([^`]+)`", r"<code>\1</code>", t)
    t = re.sub(r"\*\*([^*]+)\*\*", r"<strong>\1</strong>", t)
    t = re.sub(r"(?<!\*)\*([^*]+)\*(?!\*)", r"<em>\1</em>", t)
    t = re.sub(r"\[([^\]]+)\]\(([^)]+)\)", r'<a href="\2">\1</a>', t)
    return t


def convert(md):
    out, para, items, kind, table = [], [], [], None, []

    def flush():
        nonlocal para, items, kind, table
        if para:
            out.append("<p>" + inline(" ".join(para)) + "</p>")
        if items:
            tag = "ol" if kind == "ol" else "ul"
            out.append(f"<{tag}>" + "".join(f"<li>{inline(i)}</li>" for i in items) + f"</{tag}>")
        if table:
            rows = [r for r in table if not re.fullmatch(r"\|[\s|:-]+\|", r)]
            cells = [[c.strip() for c in r.strip("|").split("|")] for r in rows]
            head = "".join(f"<th>{inline(c)}</th>" for c in cells[0])
            body = "".join("<tr>" + "".join(f"<td>{inline(c)}</td>" for c in r) + "</tr>" for r in cells[1:])
            out.append(f"<table><thead><tr>{head}</tr></thead><tbody>{body}</tbody></table>")
        para, items, kind, table = [], [], None, []

    for line in md.splitlines():
        s = line.strip()
        if not s:
            flush()
        elif m := re.match(r"(#{1,3}) (.*)", s):
            flush()
            n = len(m.group(1))
            out.append(f"<h{n}>{inline(m.group(2))}</h{n}>")
        elif s.startswith("|"):
            table.append(s)
        elif m := re.match(r"(\*|\d+\.) (.*)", s):
            if para:
                flush()
            kind = "ol" if m.group(1)[0].isdigit() else "ul"
            items.append(m.group(2))
        elif items and line.startswith("   "):
            items[-1] += " " + s
        else:
            para.append(s)
    flush()
    return "\n".join(out)


STYLE = """
:root { --fond:#ffffff; --texte:#1b1f24; --doux:#57606a; --bleu:#003DA5; --ligne:#d8dee4; --code:#f2f4f7; }
@media (prefers-color-scheme: dark) {
  :root { --fond:#111418; --texte:#e6e9ed; --doux:#9aa4af; --bleu:#6fa0ff; --ligne:#2c333b; --code:#1d2228; }
}
* { box-sizing: border-box; }
body { margin:0; background:var(--fond); color:var(--texte);
  font:17px/1.6 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif; }
main { max-width: 760px; margin: 0 auto; padding: 32px 16px 64px; }
header { display:flex; align-items:center; gap:14px; margin-bottom: 8px; }
header img { width:56px; height:56px; }
header .nom { font-size: 1.35rem; font-weight: 700; }
header .slogan { color: var(--doux); font-size: .95rem; }
h1 { font-size: 1.7rem; margin: 24px 0 4px; line-height: 1.25; }
h2 { font-size: 1.2rem; margin: 32px 0 8px; color: var(--bleu); }
a { color: var(--bleu); }
code { background: var(--code); padding: 1px 5px; border-radius: 4px; font-size: .9em; word-break: break-word; }
table { width:100%; border-collapse: collapse; margin: 12px 0; font-size: .93rem; display:block; overflow-x:auto; }
th, td { text-align:left; vertical-align: top; padding: 8px 10px; border-bottom: 1px solid var(--ligne); }
th { white-space: nowrap; }
li { margin: 4px 0; }
footer { margin-top: 48px; color: var(--doux); font-size: .85rem; border-top: 1px solid var(--ligne); padding-top: 12px; }
"""


def page(body):
    return f"""<!doctype html>
<html lang="fr-CA">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Confidentialité · Tour de table</title>
<meta name="description" content="Politique de confidentialité de Tour de table : transcription de rencontres 100 % locale.">
<link rel="icon" href="logo.png">
<style>{STYLE}</style>
</head>
<body>
<main>
<header><img src="logo.png" alt=""><div><div class="nom">Tour de table</div>
<div class="slogan">Tout le monde parle, rien ne se perd.</div></div></header>
{body}
<footer>Tour de table — transcription de rencontres en français québécois, entièrement sur votre ordinateur.</footer>
</main>
</body>
</html>
"""


if __name__ == "__main__":
    src, dst = sys.argv[1], sys.argv[2]
    with open(src, encoding="utf-8") as f:
        md = f.read()
    with open(dst, "w", encoding="utf-8") as f:
        f.write(page(convert(md)))
