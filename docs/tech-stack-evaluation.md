# 技術スタック再評価レポート

対象: Issue #1「Initial architecture and implementation plan for CodeGloss」
調査日: 2026-08-27
目的: 「Rust で作る」という前提が本当に最適か、他により適した技術がないかを再検証する。

---

## 1. 結論（TL;DR）

**言語としての Rust は妥当。むしろ選択の余地がほとんどない。**
ただし調査の結果、**言語よりも先に見直すべき点が 2 つ**見つかった。

| # | 論点 | 判定 |
|---|------|------|
| 1 | Zed 拡張の実装言語 | **Rust 一択**。他言語の選択肢は事実上存在しない |
| 2 | LSP サーバの実装言語 | **Rust 推奨**。Go / TypeScript も可能だが、非目標（ランタイム要求の排除）と衝突する |
| 3 | Tree-sitter によるコメント抽出 | **妥当**。変更不要 |
| 4 | **表示方式（Inlay Hint 前提）** | **要見直し**。Issue のモックアップは Inlay Hint では実現できない。**Code Lens** が正解 |
| 5 | **翻訳ランタイム（ONNX Runtime + ort 前提）** | **要見直し**。v0.1 では **candle（純 Rust）** が有利。ONNX は中盤の選択肢 |
| 6 | 翻訳モデル（未定） | **候補は 2 つに絞れる**。FuguMT と Firefox Translations（en→ja） |

要するに **「Rust をやめる理由はない。やめるべきは ONNX 前提と Inlay Hint 前提」** というのが本レポートの主張である。

---

## 2. Zed 拡張の実装言語

### 結論: Rust → `wasm32-wasip1` 以外の選択肢はない

- Zed 拡張の手続き的な部分は Rust で書き、WebAssembly にコンパイルされる（[Developing Extensions](https://zed.dev/docs/extensions/developing-extensions)）。
- 拡張は `zed_extension_api` クレートの `zed::Extension` トレイトを実装する形で書く。ホストとのやり取りは WIT / `wit_bindgen` 経由（[Life of a Zed Extension](https://zed.dev/blog/zed-decoded-extensions)）。
- 「language server / context server / debugger の拡張だけが Rust コードを必要とする」と公式ドキュメントに明記されている。CodeGloss は language server 拡張なので**該当する**。

理屈の上では WIT インターフェースは言語非依存なので、WASM Component を吐ける他言語（TinyGo, Zig, ComponentizeJS 等）でも不可能ではない。しかし、

- 公式 API クレートが Rust しか存在しない
- `zed extension build` / 拡張レジストリの CI が cargo 前提

以上から、**実務上は Rust 一択**と判断してよい。ここは「Rust を選んだ」のではなく「Rust しかない」。

なお拡張側の責務は Issue 記載どおり極小で問題ない（プラットフォーム判定・バイナリの取得・起動）。`zed_extension_api` には `latest_github_release` / `download_file` / `make_file_executable` が用意されており、モデルパックやサーバ本体の初回ダウンロードはこの API で完結する。

---

## 3. LSP サーバの実装言語

拡張と違い、ここは**本当に選択の自由がある**。主要候補を比較する。

| 言語 | Tree-sitter | ローカル NMT 推論 | 配布形態 | 非目標との整合 | 総合 |
|------|------------|------------------|---------|---------------|------|
| **Rust** | 公式バインディング（`tree-sitter` crate） | candle / ort / ct2rs / llama-cpp すべて利用可 | 単一静的バイナリ | ◎ ランタイム不要 | **◎** |
| Go | cgo 経由（`go-tree-sitter`） | 実質 ONNX Runtime の cgo バインディングのみ。選択肢が痩せる | 単一バイナリ | ○ | △ |
| TypeScript / Node | `web-tree-sitter`（WASM） | **bergamot WASM がそのまま使える**のは強み | Node ランタイムが必要 | **✗**「Python 等の導入を要求しない」に抵触 | △ |
| C++ | 公式 C API 直叩き | bergamot / CTranslate2 がネイティブ | 単一バイナリ | ○ | △（開発効率が悪い） |
| Python | 良好 | 最良（研究用途） | **✗** ランタイム必須 | **✗** 非目標に明記 | ✗ |

**判定: Rust を維持する。**

決め手は言語の好みではなく、Issue が非目標に掲げた
「Ollama / LM Studio / Python / PyTorch のインストールを要求しない」
という要件である。これを満たすには「依存ゼロの単一ネイティブバイナリ」を配る必要があり、かつ推論エンジンの選択肢が最も広い言語が Rust である。

補足: TypeScript 案だけは一考の価値がある。将来の GitHub / ブラウザ拡張では bergamot の WASM ビルドがそのまま動くため、**ブラウザ側は TypeScript + bergamot WASM、エディタ側は Rust** という二本立てが最終形になる可能性は高い。ただし「コア翻訳ロジックを共有する」という Issue の想定は、この場合は成立しない点に注意（後述）。

---

## 4. 【重要】表示方式の再検討 — Inlay Hint では Issue のモックは実現できない

Issue が示している理想の見た目は次のものである。

```text
// Return cached user if available.
   ↳ キャッシュされたユーザーがあれば返します。
```

これは**訳文が独立した 1 行として表示される**形である。Zed のソースを直接読んで確認した結果、以下が判明した。

### 4.1 Inlay Hint は「行内」にしか出せない

- LSP の `InlayHint` は行内テキストであり、Zed でも `Inlay` として表示テキストに埋め込まれる（`crates/editor/src/inlays.rs:60` `Inlay::hint`）。
- したがって現実的な表示は `// Return cached user if available.  キャッシュされた…` のような**行末インライン**になる。上のモックとは別物。

### 4.2 Code Lens は「行の上のブロック」として描画される ← モック相当

Zed の Code Lens 実装を確認したところ、**行の上に独立したブロックを挿入している**。

```rust
// crates/editor/src/code_lens.rs:378
let props = BlockProperties {
    placement: BlockPlacement::Above(anchor),
    style: BlockStyle::Spacer,
    render: build_code_lens_renderer(new_line.clone(), editor_handle.clone()),
```

つまり `textDocument/codeLens` を返せば、**コメント行の上に訳文の行を出せる**。Issue のモックアップに最も近いのはこちらである。

制約:
- 表示されるテキストは `CodeLens.command.title` のみ。`command` を持たないレンズは描画されない（`crates/project/src/lsp_store/code_lens.rs:292` のコメントに明記）。→ ダミーの no-op コマンドを付ける必要がある。
- **既定値は無効**: `assets/settings/default.json:406` が `"code_lens": "off"`。`"on"` にすると行上ブロック、`"menu"` にするとコードアクションメニュー内に出る。

### 4.3 Inlay Hint も既定で無効

```jsonc
// assets/settings/default.json:788
"inlay_hints": {
  "enabled": false,          // ← 既定で off
  "show_type_hints": true,
  "show_parameter_hints": true,
  "show_other_hints": true,  // ← kind 未指定のヒントはここに属する
```

CodeGloss のヒントは `kind` を持たない（型でも引数でもない）ので `show_other_hints` の管轄になる（`crates/language/src/language_settings.rs:448` `enabled_inlay_hint_kinds()` が `None` を `show_other_hints` に対応付けている）。

→ **「インストールしただけでは何も表示されない」**。README に設定スニペットを載せることが必須。また `inlay_hints.enabled` はグローバル設定なので、有効化すると rust-analyzer の型ヒント等も一緒に出る。型ヒントが不要なユーザーは以下のようにする必要がある。

```jsonc
{
  "inlay_hints": {
    "enabled": true,
    "show_type_hints": false,
    "show_parameter_hints": false,
    "show_other_hints": true
  }
}
```

### 4.4 rust-analyzer と共存できるか → **できる（確認済み）**

これは本プロジェクト最大のアーキテクチャリスクだったが、問題ないことをソースで確認した。

1. 拡張のマニフェストに primary / secondary の区別は存在しない（`crates/extension/src/extension_manifest.rs:331` `LanguageServerManifestEntry` のフィールドは `language` / `languages` / `language_ids` / `code_action_kinds` のみ）。拡張が登録したサーバは、その言語の「利用可能なサーバ」に素直に加わる。
2. 既定設定は `"language_servers": ["..."]`（`default.json:1319`）で、`"..."` は明示されていない登録済みサーバをすべて含む（`language_settings.rs:357` `resolve_language_servers`）。→ **ユーザ設定なしで codegloss-lsp が rust-analyzer と並んで起動する。**
3. Inlay Hint はバッファに紐づく**全**サーバへ問い合わせ、`HashMap<LanguageServerId, Vec<InlayHint>>` にマージされる（`lsp_store.rs:8067` → `request_filtered_lsp_locally`、`lsp_store.rs:9639` で `language_servers_for_buffer` を全走査）。
4. Hover も `Task<Option<Vec<Hover>>>`（`lsp_store.rs:8288`）と複数形で、複数サーバの結果を扱う設計。
5. Code Lens も `for (server_id, actions) in fetched_lens` とサーバ単位で収集している（`lsp_store/code_lens.rs:236`）。

> 補足: GitHub issue [zed#15279](https://github.com/zed-industries/zed/issues/15279) に「primary/secondary の API は拡張に公開されていない」とあるが、**CodeGloss の用途では公開されている必要がない**。primary/secondary の区別が要るのはフォーマッタ等の競合機能であって、Inlay Hint / Code Lens / Hover は元々マージ前提だからである。

### 4.5 表示方式の提案

| モード | 実現手段 | 見た目 | 既定 |
|--------|---------|--------|------|
| `block`（推奨・既定にすべき） | `textDocument/codeLens` | コメント行の**上に別行** | 要 `"code_lens": "on"` |
| `inline` | `textDocument/inlayHint` | コメント**行末**に行内表示 | 要 `"inlay_hints.enabled": true` |
| `hover` | `textDocument/hover` | ホバー時のみ | 設定不要（最も導入障壁が低い） |

**`hover` は追加設定なしで動く唯一のモード**であり、v0.1 の動作確認手段として最適。Issue では hover を後回し（最終マイルストーン）にしているが、**むしろ最初に作るべき**である。

---

## 5. 翻訳ランタイムの再検討

Issue の想定は「ONNX Runtime + `ort` + Hugging Face `tokenizers`」。これは妥当な候補だが、**唯一の選択肢でも最良の初手でもない**。

### 5.1 候補比較

| 選択肢 | ビルド依存 | 推論コード量 | 速度 / サイズ | 備考 |
|--------|-----------|-------------|--------------|------|
| **A. candle（純 Rust）** | なし（cargo のみ） | **極小**。`candle-transformers` に **Marian 実装が既にある** | f32/f16 のみ。int8 なし → やや遅い・重い | クロスコンパイルが最も容易 |
| **B. ct2rs（CTranslate2）** | C++17 + CMake | **最小**。ビームサーチ内蔵 | **最速**。int8 対応 | 5 ターゲット分の C++ ビルドを CI で回す必要 |
| C. ort（ONNX Runtime） | ORT 共有ライブラリ | **大**。デコードループと KV キャッシュを自前実装 | 速い。int8 可 | `optimum` で encoder/decoder/decoder_with_past を書き出す前提 |
| D. bergamot（Firefox 実装） | C++ + CMake + intgemm | Rust バインディングが**存在しない**（自作 FFI） | **最速・最小**（int8 intgemm） | ブラウザ拡張と資産を共有できる唯一の道 |
| E. llama.cpp + 小型 LLM (GGUF) | C++ ビルド or 同梱 | 小（プロンプトのみ） | **遅い・重い** | 識別子や Markdown の保全はプロンプトで指示できるのが強み |

#### A. candle の具体的な裏付け

- `candle-transformers/src/models/marian.rs`（646 行）に `MTModel` が実装済み。KV キャッシュ（`kv_cache` / `reset_kv_cache`）まで揃っている。
- `candle-examples/examples/marian-mt/` に動く翻訳サンプルがあり、`opus-mt-*` の各設定が `marian::Config` として定義されている。
- `marian::Config` のフィールドは HuggingFace の MarianMT `config.json` と 1:1 対応（`d_model`, `encoder_layers`, `decoder_attention_heads`, `decoder_start_token_id` …）。**FuguMT は Marian アーキテクチャなので、Config を埋めるだけで載る。**
- トークナイザは `tokenizers` クレートをそのまま使える（Issue の想定どおり）。

制約: candle には Marian 向けの量子化がない。f32 で約 121MB、f16 に変換して約 60MB。ビームサーチは自前実装が必要（サンプルは貪欲法 + `LogitsProcessor`）。

#### B. ct2rs の具体的な裏付け

- CTranslate2 本体は**現役**。最新は 4.7.2（2026-05-19 リリース）で、メンテナンスは健全。
- `ct2rs` は `Translator` / `Generator` / `Whisper` をラップし、SentencePiece と HF `tokenizers` の両対応。**ビームサーチ・バッチ処理・int8 量子化がライブラリ側の責務**になるので、こちらが書くコードは最小。
- 難点は配布。macOS(arm64/x64) / Linux(x64/arm64) / Windows の 5 構成で C++17 + CMake ビルドを通す必要があり、小規模プロジェクトのリリース CI が最初に壊れるのはたいていここ。

> 公平を期すと、`tree-sitter` のグラマークレートも `cc` で C をコンパイルするため、**C コンパイラ依存は Rust 案でもどのみち発生する**。ただし「C を 1 ファイル `cc` する」のと「CMake + BLAS バックエンド選択 + intgemm/ruy を含む C++ プロジェクトをビルドする」のは難易度が 1 桁違う。

#### D. bergamot の評価

Android へ移植した実例（[blog.davidv.dev](https://blog.davidv.dev/posts/mobile-translator/)）によれば、速度は極めて良好（短文 5ms、50–100 語 20ms、200 語超 80ms、モデル約 40MB）。一方で CMake、PCRE2 欠落、AVX の不正命令、BLAS リンク（最終的に RUY へ退避）と、**移植コストは相応に高い**。Rust バインディングは現存しない。

### 5.2 推奨

> **v0.1 は A（candle）で始め、`trait Translator` の裏に隠す。**
> 実測レイテンシを取ってから B / C / D に差し替える。

理由:
1. 小規模クロスプラットフォーム OSS が最初に破綻するのは**推論速度ではなくビルドと配布**である。candle は cargo だけで 5 ターゲットに出せる。
2. Marian 実装が upstream にある以上、A の実装コストは C より明確に小さい。C は「デコードループ + `decoder_with_past` の KV 引き回し」を自分で書く必要がある。
3. 差し替えを前提にすれば、初手の選択ミスのコストは小さい。**逆に、最初から B/D を選ぶと CI 構築で数週間溶ける可能性がある。**

```rust
// crates/codegloss-translator/src/lib.rs
pub trait Translator: Send + Sync {
    fn translate(&self, segments: &[Segment]) -> anyhow::Result<Vec<String>>;
    fn model_version(&self) -> &str;   // キャッシュキーに使う
}

// v0.1: Passthrough / Candle
// v0.2 以降: Ct2 / Ort / Bergamot / ExternalEndpoint（opt-in）
```

---

## 6. モデル候補とライセンス

Issue が「最大の未解決事項」とした点。調査の結果、**現実的な候補は 2 つ**に絞れる。

| モデル | アーキテクチャ | サイズ | 品質 | ライセンス | 判定 |
|--------|--------------|--------|------|-----------|------|
| **FuguMT** (`staka/fugumt-en-ja`) | Marian | 121MB (f32) / 約 60MB (f16) | BLEU 32.7 (Tatoeba) | **CC-BY-SA-4.0** | **◎ 第一候補** |
| **Firefox Translations en→ja** | Marian（SSRU 2層デコーダ） | **59.4MB**（int8 intgemm） | **BLEU 35.3 / COMET 0.8955 (FLORES)** | **MPL-2.0** | **◎ 品質最良・要ランタイム検討** |
| NLLB-200 distilled 600M | Transformer | 大 | 良好 | **CC-BY-NC-4.0** | **✗ 非商用限定。採用不可** |
| `Helsinki-NLP/opus-mt-en-jap` | Marian | 小 | **低い**（学習データが小規模） | CC-BY-4.0 | ✗ |
| 小型 LLM (Qwen3-0.6B 等 GGUF) | Decoder-only | 0.5–2GB | 未知数 | Apache-2.0 等 | △ 将来のオプション |

### 6.1 補足

- **Firefox Translations の en→ja は 2025 年に本番投入済み**（`models/base/enja`）。メタデータ実測値: `byteSize: 59376955`, `flores.bleu: 35.3`, `comet: 0.8955`, `architecture: "base"`, `dec-cell: "ssru"`, `dec-depth: 2`, `dim-emb: 512`。CJK は品質確保のため `tiny` ではなく `base` が使われている。
  リポジトリは **MPL-2.0** なので再配布のライセンス上の摩擦が最も小さい。
- **FuguMT は CC-BY-SA-4.0（継承条項あり）**。ONNX / CTranslate2 / safetensors への変換物は二次的著作物なので、**変換後も CC-BY-SA-4.0 で、帰属表示付きで配布する必要がある**。コード（MIT）とモデルパックはリポジトリ／リリースを分け、ライセンス表記を混同させないこと。
- **NLLB は CC-BY-NC**。Issue の「redistribution/license terms」観点で明確に落ちる。将来の商用展開余地を残すなら検討対象から外すべき。

### 6.2 ジレンマ

品質・サイズは Firefox モデルが勝るが、それを動かす実装（bergamot / marian の intgemm 形式）は Rust から最も遠い。逆に FuguMT は素直な HF Marian なので candle / ct2rs / ort のどれでも動く。

→ **v0.1 は FuguMT + candle。Firefox モデルは「ブラウザ拡張フェーズで本命化する候補」として保持**、というのが素直な整理。ブラウザ拡張では bergamot WASM がそのまま使えるので、そこで初めて真価が出る。

なお Issue にある「コア翻訳ロジックをブラウザ拡張と共有する」という前提は、この整理では**部分的にしか成立しない**。共有できるのは前処理・後処理（識別子保全、Markdown/Javadoc 構造の保全、キャッシュキー設計）であり、推論エンジンは別実装になる可能性が高い。前処理・後処理を `codegloss-core` に閉じ込め、WASM ターゲットでもビルドできるようにしておけば、この共有は現実的に可能である。

---

## 7. 推奨する Issue #1 からの差分

```diff
  Zed 拡張: Rust → WASM
- ネイティブ LSP: Rust + tower-lsp-server + Tokio + Serde
+ ネイティブ LSP: Rust + tower-lsp-server + Tokio + Serde   （変更なし・妥当）

- 表示: Inlay Hint を主、Hover を後回し
+ 表示: Hover を最初に実装（設定不要で動く）
+       → Code Lens（行の上にブロック表示。モックアップ相当）
+       → Inlay Hint（行内表示）
+ README に設定スニペット必須（code_lens / inlay_hints はいずれも既定 off）

- 翻訳: ONNX Runtime + ort + tokenizers
+ 翻訳: trait Translator で抽象化
+       v0.1 = candle（純 Rust, Marian 実装が upstream にある）
+       計測後に ct2rs / ort / bergamot へ差し替え可能に

- モデル: 未定
+ モデル: FuguMT (CC-BY-SA-4.0) を第一候補
+         Firefox Translations en→ja (MPL-2.0, BLEU 35.3) を対抗馬として計測
+         NLLB-200 は CC-BY-NC のため除外
```

### 推奨マイルストーン（ML リスクを後ろに倒す）

1. Rust ワークスペース + 最小 Zed 拡張（`codegloss-lsp` を起動するだけ）
2. 最小 LSP: `initialize` / `didOpen` / `didChange`
3. **`textDocument/hover` で固定文字列を返す** ← 設定不要で疎通確認できる
4. Tree-sitter で 1 言語（Rust）のコメント抽出
5. **`textDocument/codeLens` で行の上に表示** ← モックアップの実現
6. `trait Translator` を定義し、`PassthroughTranslator`（原文をそのまま返す）で E2E を完成させる
7. **ここまで ML なしで「動く製品」にする**
8. candle + FuguMT を `CandleTranslator` として実装
9. 前処理・後処理（識別子 / バッククォート / URL / `@return` / `TODO:` の保全）
10. キャッシュ（BLAKE3、まずはインメモリ）
11. Inlay Hint モード、対応言語の拡大、設定項目

Issue のマイルストーンとの最大の違いは **6 と 7**。翻訳エンジンを入れる前に一度「完成」させることで、モデル選定という最大の不確実性を製品の完成から切り離せる。

---

## 8. 未検証・要確認事項

以下は本調査で断定できなかったもの。実装前に実機確認が必要。

1. **Inlay Hint のラベルに改行を含めた場合の描画**。LSP → 内部型の変換（`crates/project/src/lsp_command.rs:3358` `lsp_inlay_label_to_project`）に改行の除去処理は見当たらず、`inlay_map` は inlay チャンク内の改行をビットマスクで追跡している（`crates/editor/src/display_map/inlay_map.rs:435` 付近）。**複数行ヒントが描画される可能性はあるが未確認**。もし描画されるなら Code Lens を使わずモックを実現できる。
2. **Code Lens ブロックのスタイル**。`BlockStyle::Spacer` + クリック可能なレンダラであり、訳文表示に流用したときの見た目（ボタン然として見えないか）は要確認。no-op コマンドの扱いも含めて実機検証が必要。
3. Code Lens / Inlay Hint 併用時のパフォーマンス（大きなファイルでコメント数が多い場合の再計算コスト）。
4. FuguMT の実機レイテンシ（candle f32 / f16、CPU、1 コメントあたり）。インライン表示に耐えるかはここ次第。
5. Firefox の en→ja モデルを Rust から動かす現実的な経路（bergamot FFI 自作 / marian intgemm 形式から HF 形式への変換可否）。
6. 拡張が `languages = ["Java", ...]` に列挙する言語のうち、Zed 本体に無く別拡張が提供するもの（Java 等）は、その拡張が未インストールなら登録が無効になる点の挙動確認。

---

## 9. 参考資料

Zed 本体のソースは `zed-industries/zed` の `main`（2026-08-27 時点、`f66ed399`）を参照。

- [Zed: Developing Extensions](https://zed.dev/docs/extensions/developing-extensions)
- [Zed: Language Extensions](https://zed.dev/docs/extensions/languages)
- [Zed: Configuring Languages](https://zed.dev/docs/configuring-languages)
- [Zed Blog: Life of a Zed Extension — Rust, WIT, Wasm](https://zed.dev/blog/zed-decoded-extensions)
- [zed#15279 Providing primary/secondary language servers in an extension](https://github.com/zed-industries/zed/issues/15279)
- [candle: `candle-transformers/src/models/marian.rs`](https://github.com/huggingface/candle/blob/main/candle-transformers/src/models/marian.rs)
- [candle: `marian-mt` example](https://github.com/huggingface/candle/tree/main/candle-examples/examples/marian-mt)
- [ct2rs (CTranslate2 Rust bindings)](https://docs.rs/ct2rs/latest/ct2rs/) / [OpenNMT/CTranslate2](https://github.com/OpenNMT/CTranslate2)
- [ort (ONNX Runtime for Rust)](https://ort.pyke.io/)
- [browsermt/bergamot-translator](https://github.com/browsermt/bergamot-translator)
- [mozilla/firefox-translations-models](https://github.com/mozilla/firefox-translations-models)（MPL-2.0）
- [staka/fugumt-en-ja](https://huggingface.co/staka/fugumt-en-ja)（CC-BY-SA-4.0）
- [Firefox now supports Chinese, Japanese, and Korean translation](https://blog.mozilla.org/en/firefox/cjk-translation-on-android/)
- [Using local translation models on Android](https://blog.davidv.dev/posts/mobile-translator/)
