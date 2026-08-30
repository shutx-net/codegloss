# CodeGloss

英語のコメントを、ソースコードを書き換えずに日本語で読むための補助ツールです。

```java
// Return cached user if available.
   ↳ キャッシュされたユーザーがあれば返します。
public User findUser(String id) {
```

日本語が見えているのはエディタの表示だけで、ファイルの中身は 1 文字も変わりません。コピーすれば元のコードのままです。

## ねらい

- **ソースを汚さない** — 訳文はエディタ上の表示のみ。差分にもコピーにも現れない
- **ローカルで翻訳** — コードを外部サービスに送信しない
- **準備がいらない** — 翻訳モデルは初回に自動で取得。Python や API キーの用意は不要

## 状況

開発初期です。Zed 拡張と LSP サーバの骨組みができ、Rust のコメントを Tree-sitter で
抜き出して、ホバーと Code Lens（コメント行の上に出る独立した行）に返すところまで
動きます。**翻訳も実際に動きます**（candle + FuguMT。CPU で 1 段落あたり
0.2 秒ほど）。

ただし**モデルの自動取得はまだありません。**モデルパックを自分で作って
`--model-pack` で渡す必要があり、渡さなければ英語のまま表示されます（サーバは
落ちません）。作り方は
[tools/convert-fugumt/README.md](tools/convert-fugumt/README.md) を参照してください。

```sh
pip install -r tools/convert-fugumt/requirements.txt
python3 tools/convert-fugumt/convert.py ~/codegloss-model
cargo build --release -p codegloss-lsp --features candle
./target/release/codegloss-lsp --model-pack ~/codegloss-model
```

翻訳エンジンは差し替え可能な形（`trait Translator`）のままです。

- 設計方針: [Issue #1](https://github.com/shutx-net/codegloss/issues/1)
- 技術選定の根拠: [docs/tech-stack-evaluation.md](docs/tech-stack-evaluation.md)
- 翻訳の実測値（速度・メモリ・訳文の例）: [docs/model-runtime-notes.md](docs/model-runtime-notes.md)

最初のターゲットは Zed 拡張です。将来的には他のエディタや、GitHub 上でソースを読むためのブラウザ拡張も見据えています。

## 表示方法と設定

CodeGloss は 3 通りの表示方法を想定しています。

| 表示方法 | 見え方 | 必要な Zed の設定 | 状況 |
|---|---|---|---|
| ホバー | コメントにカーソルを合わせると訳文が出る（下に原文を引用） | 不要 | 実装済み |
| Code Lens | コメント行の上に、独立した行として訳文が出る | `"code_lens": "on"` | 実装済み |
| Inlay Hint | コメント行の行内に訳文が出る | `"inlay_hints": { "enabled": true }` | 未実装 |

Zed の既定値は `"code_lens": "off"` と `"inlay_hints": { "enabled": false }`
です。**拡張をインストールしただけでは Code Lens と Inlay Hint は表示されません。**

設定の書き方は [DEVELOPERS.md の「表示方法の設定」](DEVELOPERS.md#表示方法の設定)
にまとめてあります。**どのファイルに書くかで効いたり効かなかったりする**ので、
設定の記述はそちらに一本化しています。

### 訳ができるまでの表示（ホバーと Code Lens で違います）

翻訳は LSP のリクエストの中では実行しません。翻訳には時間がかかり、リクエストの
中で待つとエディタごと固まるためです。コメントはファイルを開いた時点で
バックグラウンドの翻訳待ち行列に入り、訳ができ次第キャッシュに入ります。

訳がまだ無いときの表示は、**ホバーと Code Lens でわざと変えてあります。**

| | 訳が無いとき | 訳が届いたら |
|---|---|---|
| ホバー | 英語の原文をそのまま表示 | 次にカーソルを合わせたときに訳文 |
| Code Lens | `⟳ 翻訳中…` と表示 | 自動的に訳文へ差し替わる |

一貫していないように見えますが、置かれる場所が違うためです。

- **Code Lens は原文のすぐ上の行に出ます。** ここに原文を出すと、上下の行に
  同じ英文が 2 回並ぶだけでノイズにしかなりません。逆にホバーのポップアップは
  コードを覆って出るので、そこに見えている原文が読者にとって唯一の原文です。
- **Code Lens は後から差し替えられます。** 訳ができた時点でサーバが
  `workspace/inlayHint/refresh` と `workspace/codeLens/refresh` を送り、
  エディタに取り直させます。プレースホルダが出ているのは一瞬です。
- **ホバーにはこの仕組みがありません。** LSP に `workspace/hover/refresh` は
  存在せず、いちど表示したポップアップを後から差し替える手段がないためです。
  「翻訳中…」と出したまま直せないより、原文が読める方がましだと判断しました。
- Code Lens で訳が届くまで**何も出さない**という選択肢も取っていません。訳が
  届いた瞬間に行が 1 行挿入されて、読んでいるコードの位置が飛ぶためです。
  プレースホルダは行の場所を先に押さえる役割も持っています。

## 開発

[DEVELOPERS.md](DEVELOPERS.md) を参照してください。

## ライセンス

[MIT](LICENSE)
