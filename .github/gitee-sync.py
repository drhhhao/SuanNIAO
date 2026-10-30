"""发版后把这一版放到 Gitee 上（release.yml 的「同步到 Gitee」）：国内连 GitHub 慢，两端检查更新先找 Gitee。

做的事：
1. 在 Gitee 仓库（GITEE_REPO）建同名的发行版（已经有了就接着用，重跑不会重复传）
2. 传上电脑安装包、拆成几份的手机安装包、不带模型的手机更新包、latest.json.sig，最后传 latest.json
   （Gitee 单个附件最多 100 MB，手机安装包一百六十多 MB；拆法和 latest.json 里 android.parts 写的一样，对不上就不传）
3. 不带令牌下载回 latest.json 和签名，和刚传的一样才算成（两端就是这样读的）
4. 只留最近 KEEP 个发行版，旧的连标签一起删掉（一个仓库的附件总共最多 1 GB）

两端从 https://gitee.com/<仓库>/releases/download/latest/latest.json 读最新一版（Gitee 的 latest 指最新建的发行版），
文件从 latest.json 的 mirror（这一版的下载地址）下。安装包不管从哪下，都按 latest.json 核对大小和 SHA-256；
电脑端还核对 latest.json 的签名，手机端核对安装包的签名。

只用 Python 自带的库和 curl。令牌只放在请求里，不打印。
"""
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

REPO = os.environ["GITEE_REPO"]
TOKEN = os.environ.get("GITEE_TOKEN", "")
TAG = os.environ["TAG"]
DIST = "dist"
API = f"https://gitee.com/api/v5/repos/{REPO}"
KEEP = 3


def fail(text):
    print(f"::error::{text}")
    sys.exit(1)


def api(method, path, form=None):
    """调 Gitee 的接口。文字用表单编码传（Gitee 收带中文的 JSON 会回一个 HTML 错误页）"""
    form = dict(form or {}, access_token=TOKEN)
    url = API + path
    data = None
    if method == "GET":
        url += "?" + urllib.parse.urlencode(form)
    else:
        data = urllib.parse.urlencode(form).encode()
    request = urllib.request.Request(url, data=data, method=method)
    try:
        with urllib.request.urlopen(request, timeout=60) as response:
            body = response.read()
            return json.loads(body) if body.strip() else None
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        fail(f"Gitee 接口 {method} {path} 返回 {e.code}：{e.read()[:300].decode('utf-8', 'replace')}")


def upload(release_id, path, name):
    """传一个附件（multipart，curl 传，失败重试三次）"""
    url = f"{API}/releases/{release_id}/attach_files"
    for attempt in range(3):
        result = subprocess.run(
            ["curl", "-sS", "--max-time", "1200", "-X", "POST", url,
             "-F", f"access_token={TOKEN}", "-F", f"file=@{path};filename={name}"],
            capture_output=True, text=True)
        try:
            answer = json.loads(result.stdout)
        except ValueError:
            answer = {}
        if answer.get("name") == name and answer.get("size") == os.path.getsize(path):
            print(f"传好了 {name}（{answer['size']} 字节）")
            return
        print(f"传 {name} 没成（第 {attempt + 1} 次）：{result.stdout[:200]} {result.stderr[:200]}")
        time.sleep(10)
    fail(f"{name} 传不上 Gitee")


def fetch(url):
    """不带令牌下载（两端就是这样下的），跟着跳转"""
    request = urllib.request.Request(url, headers={"User-Agent": "SuanNIAO-sync"})
    with urllib.request.urlopen(request, timeout=120) as response:
        return response.read()


def main():
    if not TOKEN:
        fail("仓库的加密变量里没有 GITEE_TOKEN")
    manifest_bytes = open(f"{DIST}/latest.json", "rb").read()
    manifest = json.loads(manifest_bytes)
    mirror = f"https://gitee.com/{REPO}/releases/download/{TAG}/"
    if manifest.get("tag") != TAG or manifest.get("mirror") != mirror:
        fail(f"latest.json 的 tag / mirror 和这次同步的对不上：{manifest.get('tag')} {manifest.get('mirror')}")

    # 要传的文件，按顺序：安装包在前，latest.json 最后（它一出现，两端就会来下安装包）
    files = []
    pc = manifest["pc"]
    files.append((f"{DIST}/{pc['file']}", pc["file"]))
    android = manifest["android"]
    apk = open(f"{DIST}/{android['file']}", "rb").read()
    offset = 0
    for part in android["parts"]:
        chunk = apk[offset:offset + part["size"]]
        if len(chunk) != part["size"]:
            fail(f"手机安装包拆不出 {part['file']}")
        path = f"{DIST}/{part['file']}"
        with open(path, "wb") as f:
            f.write(chunk)
        files.append((path, part["file"]))
        offset += part["size"]
    if offset != len(apk) or len(apk) != android["size"]:
        fail("手机安装包拆开的几份加起来和原来的不一样大")
    # 不带语音模型的更新包（三十 MB 左右）：不拆，整个传
    lite = android.get("lite")
    if lite:
        if os.path.getsize(f"{DIST}/{lite['file']}") != lite["size"]:
            fail(f"{lite['file']} 和 latest.json 里写的不一样大")
        files.append((f"{DIST}/{lite['file']}", lite["file"]))
    files.append((f"{DIST}/latest.json.sig", "latest.json.sig"))
    files.append((f"{DIST}/latest.json", "latest.json"))

    release = api("GET", f"/releases/tags/{TAG}")
    if not release:
        body = (f"{manifest['title']}。国内下载用的镜像，和 GitHub 上的同一版：{manifest['page']}\n\n"
                "装着蒜鸟的电脑、手机检查更新时会先从这里下载。手机安装包太大，拆成了几份（.apk.1、.apk.2），"
                "蒜鸟自己会拼回来；-update.apk 是不带语音模型的更新包，给 App 里自动更新用。"
                "要手动下载安装包，请去上面的 GitHub 页面（第一次装手机版要下完整的那个）。")
        release = api("POST", "/releases", {"tag_name": TAG, "name": manifest["title"], "body": body,
                                             "target_commitish": "master", "prerelease": "false"})
        if not release:
            fail("Gitee 上建不了发行版")
        print(f"在 Gitee 上建了发行版 {TAG}（{release['id']}）")
    have = {a.get("name"): a.get("size") for a in api("GET", f"/releases/{release['id']}/attach_files") or []}
    for path, name in files:
        if have.get(name) == os.path.getsize(path):
            print(f"已经有 {name}，不再传")
            continue
        upload(release["id"], path, name)

    # 像两端一样不带令牌读回来（刚传完可能要等一会儿）
    expected = {"latest.json": manifest_bytes, "latest.json.sig": open(f"{DIST}/latest.json.sig", "rb").read()}
    for name, data in expected.items():
        for url in (mirror + name, f"https://gitee.com/{REPO}/releases/download/latest/{name}"):
            for attempt in range(6):
                try:
                    if fetch(url) == data:
                        print(f"读回来对得上：{url}")
                        break
                except Exception as e:  # noqa: BLE001 —— 等一会儿再读
                    print(f"读 {url} 没成：{e}")
                time.sleep(10)
            else:
                fail(f"从 Gitee 读回来的 {url} 和传上去的不一样")

    # 只留最近 KEEP 个
    releases = api("GET", "/releases", {"per_page": 100}) or []
    releases.sort(key=lambda r: r.get("created_at", ""), reverse=True)
    for old in releases[KEEP:]:
        if old["tag_name"] == TAG:
            continue
        api("DELETE", f"/releases/{old['id']}")
        owner = REPO.split("/")[0]
        subprocess.run(["git", "-c", "credential.helper=", "push", "-q",
                        f"https://{owner}:{TOKEN}@gitee.com/{REPO}.git", f":refs/tags/{old['tag_name']}"],
                       capture_output=True)
        print(f"删掉了旧的 {old['tag_name']}")


if __name__ == "__main__":
    main()
