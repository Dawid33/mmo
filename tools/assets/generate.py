#!/usr/bin/env python3
"""Game models from a text prompt: a Leonardo.ai picture of the thing alone on
a flat key colour, keyed out, then 3D AI Studio's TRELLIS.2 image-to-3D, which
returns a textured GLB the client loads as-is (Bevy reads glTF). Adapted from
~/Projects/mod's realchain tools (generate_assets.py, model3d.py).

python3 tools/assets/generate.py [--only NAME ...] [--picture-only] [--force] [--list] [--balance]

Keys: LEONARDO_API_KEY and THREEDAISTUDIO_API_KEY in the repo root's .env
(git-ignored, never commit it). Downloads are kept in tools/assets/raw/
(git-ignored) and never asked for twice unless --force, so a rerun costs
nothing: a picture is a few Leonardo tokens, a model 30 3D AI Studio credits.
Finished models are written to assets/models/<name>.glb (committed).

The model copies the picture faithfully, wrong geometry included, so look at
tools/assets/raw/<name>.png before paying for the model (--picture-only).
"""
import argparse
import base64
import io
import json
import os
import shutil
import sys
import time
import urllib.error
import urllib.request

from PIL import Image, ImageFilter

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
RAW = os.path.join(HERE, "raw")
MODELS = os.path.join(REPO, "assets", "models")

LEONARDO = "https://cloud.leonardo.ai/api/rest"
LEONARDO_MODEL = "openai/gpt-image-2.5-sunburst"
PICTURE_SIZE = 2048
STUDIO = "https://api.3daistudio.com"

KEYS = {"green": "pure bright green (#00FF00)", "magenta": "pure bright magenta (#FF00FF)"}

# One look for every model: the game is a voxel island seen from an RTS camera,
# so models are chunky, readable from above and far away, and neutral in colour
# where the team colour will go (the client tints it).
STYLE = ("{what}, as a 3D game model for a top-down real-time strategy game: stylized, chunky, slightly "
         "exaggerated proportions and a bold readable silhouette, simple hand-painted textures, the parts that "
         "show the team colour painted plain light grey. Shown whole in a three-quarter view from the front and "
         "slightly above, {pose}, evenly lit with soft light from the upper left. It stands alone in the middle of "
         "the picture on a perfectly flat, uniform, {key} background: no ground, no cast shadow, no text, no "
         "border, nothing touching the edges of the picture, none of the background colour on it.")


def model(what, pose="standing still in a neutral pose", key="green", faces=12000, texture=1024):
    """faces: the decimation target — an RTS shows many units at once, so
    units stay low; texture: the model's texture edge in pixels."""
    return {"prompt": STYLE.format(what=what, pose=pose, key=KEYS[key]), "key": key, "faces": faces,
            "texture": texture}


# The MVP's models. Add an entry, run with --only NAME --picture-only, look at
# the picture, then run again without --picture-only for the model.
ASSETS = {
    "soldier": model("a sturdy fantasy foot soldier in simple plate armour with a short sword and a round shield",
                     pose="standing in a relaxed ready stance, feet apart, arms slightly away from the body"),
    "archer": model("a nimble fantasy archer in a hooded leather tunic holding a longbow",
                    pose="standing in a relaxed ready stance, feet apart, arms slightly away from the body"),
    "barracks": model("a small fortified barracks building of timber and stone with a pitched roof, a wide open "
                      "doorway and a banner pole", pose="a single building on a small square stone footing",
                      faces=30000, texture=2048),
    "core": model("a team's main base: a squat stone keep with a glowing crystal at its top",
                  pose="a single building on a small square stone footing", faces=30000, texture=2048),
}


def env_key(name):
    path = os.path.join(REPO, ".env")
    try:
        with open(path) as f:
            for line in f:
                if line.startswith(name + "="):
                    return line.split("=", 1)[1].strip()
    except FileNotFoundError:
        pass
    sys.exit("%s not found in %s" % (name, path))


def request(base, method, path, key, body=None, auth="Bearer "):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(base + path, data=data, method=method, headers={
        "accept": "application/json", "content-type": "application/json", "authorization": auth + key})
    try:
        with urllib.request.urlopen(req, timeout=120) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        sys.exit("%s %s%s: %s %s" % (method, base, path, e.code, e.read().decode(errors="replace")))


def download(url, path):
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0 (asset tool)"})
    with urllib.request.urlopen(req, timeout=300) as r, open(path, "wb") as f:
        shutil.copyfileobj(r, f)
    return path


# --- Leonardo: the picture ---------------------------------------------------

def picture(name, force=False):
    path = os.path.join(RAW, name + ".png")
    if os.path.exists(path) and not force:
        return path
    key = env_key("LEONARDO_API_KEY")
    body = {"public": False, "model": LEONARDO_MODEL, "parameters": {
        "quality": "HIGH", "prompt": ASSETS[name]["prompt"], "quantity": 1,
        "width": PICTURE_SIZE, "height": PICTURE_SIZE, "prompt_enhance": "OFF"}}
    response = request(LEONARDO, "POST", "/v2/generations", key, body)
    gid = next((v["generationId"] for v in (response.get("generate") or {}, response.get("sdGenerationJob") or {},
                                            response) if isinstance(v, dict) and v.get("generationId")), None)
    if not gid:
        sys.exit("no generationId in " + json.dumps(response))
    print(name, "picture generation", gid, flush=True)
    deadline = time.time() + 900
    while time.time() < deadline:
        g = request(LEONARDO, "GET", "/v1/generations/" + gid, key).get("generations_by_pk") or {}
        if g.get("status") == "COMPLETE" and g.get("generated_images"):
            download(g["generated_images"][0]["url"], path)
            print(name, "->", path, flush=True)
            return path
        if g.get("status") == "FAILED":
            sys.exit(gid + ": generation failed: " + json.dumps(g))
        time.sleep(5)
    sys.exit(gid + ": timed out")


def key_out(img, key="green"):
    """The key colour made transparent, its spill taken off the edges."""
    img = img.convert("RGBA")
    px = img.load()
    w, h = img.size
    for y in range(h):
        for x in range(w):
            r, g, b, _ = px[x, y]
            spill = g - max(r, b) if key == "green" else min(r, b) - g
            if spill > 90:
                px[x, y] = (0, 0, 0, 0)
            elif spill > 25:
                a = int(255 * (90 - spill) / 65)
                px[x, y] = (r, max(r, b), b, a) if key == "green" else (min(r, g + 20), g, min(b, g + 20), a)
            elif spill > 0:
                px[x, y] = (r, max(r, b), b, 255) if key == "green" else (min(r, g + 20), g, min(b, g + 20), 255)
    alpha = img.getchannel("A").filter(ImageFilter.MinFilter(3)).filter(ImageFilter.GaussianBlur(0.8))
    img.putalpha(alpha)
    box = img.getchannel("A").point(lambda v: 255 if v > 16 else 0).getbbox()
    return img.crop(box) if box else img


# --- 3D AI Studio: the model -------------------------------------------------

def model3d(name, force=False):
    out = os.path.join(MODELS, name + ".glb")
    raw = os.path.join(RAW, name + ".glb")
    if os.path.exists(raw) and not force:
        shutil.copyfile(raw, out)
        return out
    item = ASSETS[name]
    keyed = key_out(Image.open(os.path.join(RAW, name + ".png")), item["key"])
    keyed.save(os.path.join(RAW, name + "-keyed.png"))
    buf = io.BytesIO()
    keyed.save(buf, "PNG")
    key = env_key("THREEDAISTUDIO_API_KEY")
    body = {"image": "data:image/png;base64," + base64.b64encode(buf.getvalue()).decode(), "resolution": "1024",
            "textures": True, "texture_size": item["texture"], "decimation_target": item["faces"]}
    task = request(STUDIO, "POST", "/v1/3d-models/trellis2/generate/", key, body)["task_id"]
    print(name, "model task", task, flush=True)
    deadline = time.time() + 1800
    while time.time() < deadline:
        st = request(STUDIO, "GET", "/v1/generation-request/%s/status/" % task, key)
        if st.get("status") == "FINISHED":
            url = next(r["asset"] for r in st["results"] if r.get("asset_type") == "3D_MODEL")
            download(url, raw)
            shutil.copyfile(raw, out)
            print(name, "->", out, os.path.getsize(out), "bytes", flush=True)
            return out
        if st.get("status") in ("FAILED", "ERROR") or st.get("failure_reason"):
            sys.exit("%s: %s" % (task, json.dumps(st)))
        time.sleep(10)
    sys.exit(task + ": timed out")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--only", nargs="+", metavar="NAME", help="just these assets")
    ap.add_argument("--picture-only", action="store_true", help="stop after the Leonardo picture")
    ap.add_argument("--force", action="store_true", help="ask again even if a download exists")
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--balance", action="store_true", help="credits left on both services")
    args = ap.parse_args()
    if args.list:
        for name, item in ASSETS.items():
            have = [k for k, p in (("picture", name + ".png"), ("model", name + ".glb"))
                    if os.path.exists(os.path.join(RAW, p))]
            print("%-10s faces=%-6d texture=%-5d have=%s" % (name, item["faces"], item["texture"], ",".join(have) or "-"))
        return
    if args.balance:
        me = request(LEONARDO, "GET", "/v1/me", env_key("LEONARDO_API_KEY"))
        details = (me.get("user_details") or [{}])[0]
        print("leonardo api tokens:", details.get("apiPaidTokens"))
        print("3daistudio:", request(STUDIO, "GET", "/account/user/wallet/", env_key("THREEDAISTUDIO_API_KEY")))
        return
    names = args.only or list(ASSETS)
    unknown = [n for n in names if n not in ASSETS]
    if unknown:
        sys.exit("unknown assets: %s (see --list)" % ", ".join(unknown))
    os.makedirs(RAW, exist_ok=True)
    os.makedirs(MODELS, exist_ok=True)
    for name in names:
        picture(name, args.force)
        if not args.picture_only:
            model3d(name, args.force)


if __name__ == "__main__":
    main()
