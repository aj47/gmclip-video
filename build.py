"""Stage only public assets for Cloudflare Pages."""
from pathlib import Path
import shutil

root = Path(__file__).resolve().parent
out = root / "dist"
out.mkdir(exist_ok=True)
for name in ("index.html", "styles.css", "signup.js"):
    shutil.copy2(root / name, out / name)
shutil.copytree(root / "assets", out / "assets", dirs_exist_ok=True)
(out / "_routes.json").write_text('{"version":1,"include":["/api/*"],"exclude":[]}\n')
print(out)
