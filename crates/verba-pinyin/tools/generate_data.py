#!/usr/bin/env python3
"""生成 Verba 拼音词库（chars.txt / words.txt）。

数据源：
- 单字拼音：mozillazg/pinyin-data 的 pinyin.txt（Unihan 聚合，MIT）
- 单字频率：ruddfawcett/hanziDB.csv（基于 Jun Da 现代汉语字频表）
- 词语拼音：mozillazg/phrase-pinyin-data 的 cc_cedict.txt（CC-CEDICT）

输出格式（每行一个无调拼音）：
  pinyin<TAB>rank,text rank,text ...   # 按 rank 升序（越小越常用）
"""
import io, os, urllib.request

BASE = os.path.dirname(os.path.abspath(__file__))
DATA = os.path.join(BASE, "..", "data")

def fetch(url):
    print("fetch", url)
    with urllib.request.urlopen(url, timeout=30) as r:
        return r.read().decode("utf-8")

TONE_MAP = str.maketrans({
    "ā":"a","á":"a","ǎ":"a","à":"a",
    "ē":"e","é":"e","ě":"e","è":"e",
    "ī":"i","í":"i","ǐ":"i","ì":"i",
    "ō":"o","ó":"o","ǒ":"o","ò":"o",
    "ū":"u","ú":"u","ǔ":"u","ù":"u",
    "ǖ":"v","ǘ":"v","ǚ":"v","ǜ":"v","ü":"v",
    "ê":"e","ń":"n","ň":"n","ǹ":"n",
})

def strip_tones(py):
    return py.translate(TONE_MAP).replace(" ", "").replace("'", "").lower()

def is_cjk(ch):
    return "\u4e00" <= ch <= "\u9fff"

def main():
    os.makedirs(DATA, exist_ok=True)

    # ---- 单字：pinyin.txt（所有读音）+ hanzi_db.csv（频率排名） ----
    pinyin_txt = fetch("https://raw.githubusercontent.com/mozillazg/pinyin-data/master/pinyin.txt")
    char_pinyins = {}   # char -> set(pinyin_no_tone)
    for line in pinyin_txt.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        # U+XXXX: py1,py2  # hanzi
        try:
            codepoint, rest = line.split(":", 1)
            pys, _ = rest.split("#", 1)
        except ValueError:
            continue
        try:
            ch = chr(int(codepoint.strip().replace("U+", ""), 16))
        except ValueError:
            continue
        if not is_cjk(ch):
            continue
        for py in pys.split(","):
            py = strip_tones(py)
            if py:
                char_pinyins.setdefault(ch, set()).add(py)

    hanzi = fetch("https://raw.githubusercontent.com/ruddfawcett/hanziDB.csv/master/hanzi_db.csv")
    char_rank = {}
    for line in hanzi.splitlines()[1:]:
        parts = line.split(",")
        if len(parts) < 3:
            continue
        try:
            rank = int(parts[0])
        except ValueError:
            continue
        ch = parts[1]
        char_rank[ch] = rank

    # 单字：pinyin -> [(rank, char)]
    char_map = {}
    for ch, pys in char_pinyins.items():
        rank = char_rank.get(ch, 10000 + ord(ch))
        for py in pys:
            char_map.setdefault(py, []).append((rank, ch))
    for py in char_map:
        char_map[py].sort()

    # ---- 词语：cc_cedict.txt ----
    cedict = fetch("https://raw.githubusercontent.com/mozillazg/phrase-pinyin-data/master/cc_cedict.txt")
    word_map = {}
    for line in cedict.splitlines():
        line = line.strip()
        if not line or line.startswith("#") or ":" not in line:
            continue
        word, pys = line.split(":", 1)
        word = word.strip()
        pys = pys.strip()
        if not (2 <= len(word) <= 5) or not all(is_cjk(c) for c in word):
            continue
        py = strip_tones(pys)
        if not py or len(py) < 2:
            continue
        # 词语频率启发式：取组成字的最大字频排名（词常用度取决于最生僻的字，越小越常用）
        ranks = [char_rank.get(c, 10000 + ord(c)) for c in word]
        rank = max(ranks)
        word_map.setdefault(py, []).append((rank, word))
    for py in word_map:
        word_map[py].sort()
        word_map[py] = word_map[py][:40]  # 每个拼音最多保留 40 个词语

    # ---- 写出 ----
    def write(name, data):
        path = os.path.join(DATA, name)
        with io.open(path, "w", encoding="utf-8", newline="\n") as f:
            for py in sorted(data):
                entries = " ".join(f"{r},{t}" for r, t in data[py])
                f.write(f"{py}\t{entries}\n")
        print(f"{name}: {len(data)} pinyins")

    write("chars.txt", char_map)
    write("words.txt", word_map)

    readme = """# 词库数据与来源

- `chars.txt`：单字候选。拼音（无调）→ 按频率排序的单字。
  数据来源：`mozillazg/pinyin-data`（pinyin.txt，MIT）提供读音；
  `ruddfawcett/hanziDB.csv`（基于 Jun Da 现代汉语字频表）提供频率排序。
- `words.txt`：词语候选。拼音（无调）→ 按"最小字频"启发式排序的词语（每拼音≤40 条）。
  数据来源：`mozillazg/phrase-pinyin-data` 的 `cc_cedict.txt`（CC-CEDICT，CC BY-SA 4.0）。

## 许可
- mozillazg/pinyin-data：MIT License
- hanziDB.csv：见 https://github.com/ruddfawcett/hanziDB.csv （基于 Jun Da 字频表）
- CC-CEDICT：Creative Commons Attribution-ShareAlike 4.0
  https://cc-cedict.org/ （MDBG / CC-CEDICT contributors）
"""
    with io.open(os.path.join(DATA, "README.md"), "w", encoding="utf-8", newline="\n") as f:
        f.write(readme)
    print("done")

if __name__ == "__main__":
    main()
