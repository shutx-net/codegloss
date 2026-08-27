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

開発初期です。設計と技術選定を終えた段階で、まだ動くものはありません。

- 設計方針: [Issue #1](https://github.com/shutx-net/codegloss/issues/1)
- 技術選定の根拠: [docs/tech-stack-evaluation.md](docs/tech-stack-evaluation.md)

最初のターゲットは Zed 拡張です。将来的には他のエディタや、GitHub 上でソースを読むためのブラウザ拡張も見据えています。

## 開発

[DEVELOPERS.md](DEVELOPERS.md) を参照してください。

## ライセンス

[MIT](LICENSE)
