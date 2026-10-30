"""发版后在本机把这一版传到 Gitee（国内镜像）：python tools/gitee-upload.py [发布名]

GitHub 的机器在国外，从那边往 Gitee 传一百多 MB 半小时都传不完（2026-10-31），所以改成发版后在本机传。
做的事：用 gh 把 GitHub 上这一版（不写发布名就是最新一版）的安装包、latest.json 和签名下到临时文件夹，
核对大小和 SHA-256，再交给 .github/gitee-sync.py 传到 Gitee、读回核对、删掉旧的发行版。

令牌从 E:\\Documents\\蒜鸟发版密钥\\gitee-token.txt 读（不进仓库、不打印），也可以用环境变量 GITEE_TOKEN。
要先装 GitHub CLI（gh）并登录过。
"""
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GH_REPO = "drhhhao/SuanNIAO"
GITEE_REPO = "drhhhao_0/SuanNIAO"
TOKEN_FILE = r"E:\Documents\蒜鸟发版密钥\gitee-token.txt"
GH = shutil.which("gh") or r"C:\Program Files\GitHub CLI\gh.exe"


def gh(*args):
    return subprocess.run([GH, *args, "--repo", GH_REPO], check=True, capture_output=True, text=True, encoding="utf-8").stdout


def main():
    token = os.environ.get("GITEE_TOKEN") or (open(TOKEN_FILE, encoding="utf-8").read().strip() if os.path.exists(TOKEN_FILE) else "")
    if not token:
        sys.exit(f"没有 Gitee 令牌：放到 {TOKEN_FILE}，或者设环境变量 GITEE_TOKEN")
    tag = sys.argv[1] if len(sys.argv) > 1 else json.loads(gh("release", "view", "--json", "tagName"))["tagName"]
    work = tempfile.mkdtemp(prefix="suanniao-gitee-")
    dist = os.path.join(work, "dist")
    print(f"从 GitHub 下载 {tag} …")
    gh("release", "download", tag, "--dir", dist, "--pattern", "*.exe", "--pattern", "*.apk", "--pattern", "latest.json*")
    manifest = json.load(open(os.path.join(dist, "latest.json"), encoding="utf-8"))
    if manifest.get("tag") != tag:
        sys.exit(f"latest.json 是 {manifest.get('tag')} 的，不是 {tag}")
    for item in (manifest["pc"], manifest["android"], manifest["android"].get("lite")):
        if not item:
            continue
        data = open(os.path.join(dist, item["file"]), "rb").read()
        if len(data) != item["size"] or hashlib.sha256(data).hexdigest() != item["sha256"]:
            sys.exit(f"{item['file']} 和 latest.json 里写的对不上")
        print(f"核对过 {item['file']}（{len(data)} 字节）")
    env = dict(os.environ, GITEE_TOKEN=token, GITEE_REPO=GITEE_REPO, TAG=tag, PYTHONIOENCODING="utf-8")
    code = subprocess.run([sys.executable, os.path.join(ROOT, ".github", "gitee-sync.py")], cwd=work, env=env).returncode
    shutil.rmtree(work, ignore_errors=True)
    sys.exit(code)


if __name__ == "__main__":
    main()
