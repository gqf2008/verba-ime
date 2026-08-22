# 词库数据与来源

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
