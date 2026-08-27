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

開発初期です。Zed 拡張と LSP サーバの骨組みができ、ホバーに固定の文字列を
返すところまで動きます。コメントの抽出も翻訳もまだ実装していません。

- 設計方針: [Issue #1](https://github.com/shutx-net/codegloss/issues/1)
- 技術選定の根拠: [docs/tech-stack-evaluation.md](docs/tech-stack-evaluation.md)

最初のターゲットは Zed 拡張です。将来的には他のエディタや、GitHub 上でソースを読むためのブラウザ拡張も見据えています。

## 表示方法と設定

CodeGloss は 3 通りの表示方法を想定しています。

| 表示方法 | 見え方 | 必要な Zed の設定 |
|---|---|---|
| ホバー | コメントにカーソルを合わせると訳文が出る | 不要 |
| Code Lens | コメント行の上に、独立した行として訳文が出る | `"code_lens": "on"` |
| Inlay Hint | コメント行の行内に訳文が出る | `"inlay_hints": { "enabled": true }` |

Zed の既定値は `"code_lens": "off"` と `"inlay_hints": { "enabled": false }`
です。**拡張をインストールしただけでは Code Lens と Inlay Hint は表示されません。**
設定スニペットは、それぞれの表示方法を実装した段階でここに載せます。

現時点で動くのはホバーだけで、内容も固定の文字列です。

## 開発

[DEVELOPERS.md](DEVELOPERS.md) を参照してください。

## ライセンス

[MIT](LICENSE)
