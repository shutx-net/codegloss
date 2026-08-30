#!/usr/bin/env python3
"""FuguMT を CodeGloss のモデルパックに変換する。

出力は 1 つのディレクトリで、`codegloss-lsp --model-pack <dir>` がそのまま読む。

    manifest.json          パックの素性（model_id / model_version / license / attribution）
    config.json            上流のものをそのままコピー
    pytorch_model.bin      上流の重み。candle が pickle を直接読むので変換しない
    tokenizer-source.json  source.spm から生成した高速トークナイザ
    tokenizer-target.json  target.spm から生成した高速トークナイザ
    LICENSE                CC-BY-SA-4.0 の全文
    NOTICE                 帰属表示

IMPORTANT: 出力はこのリポジトリにコミットしない（AGENTS.md）。重みは
CC-BY-SA-4.0 で、コードの MIT とは別物として配る。

使い方:

    pip install -r requirements.txt
    python3 convert.py /path/to/pack            # Hugging Face から取得する
    python3 convert.py /path/to/pack --from-dir /path/to/downloaded
    python3 convert.py /path/to/pack --verify   # 低速トークナイザと突き合わせる
                                                # （transformers<5 が要る）
"""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import sys
import urllib.request
from pathlib import Path

# 変換元。CC-BY-SA-4.0。
MODEL_ID = "staka/fugumt-en-ja"
BASE_URL = f"https://huggingface.co/{MODEL_ID}/resolve/main"

# 上流から取ってくるファイル。model.safetensors も tokenizer.json も
# 公開されていないので、この一覧が上流の全てになる。
UPSTREAM_FILES = (
    "config.json",
    "generation_config.json",
    "pytorch_model.bin",
    "source.spm",
    "target.spm",
    "vocab.json",
)

# CC-BY-SA-4.0 の全文。1 本目が駄目なら 2 本目を試す。
LICENSE_URLS = (
    "https://creativecommons.org/licenses/by-sa/4.0/legalcode.txt",
    "https://raw.githubusercontent.com/spdx/license-list-data/main/text/CC-BY-SA-4.0.txt",
)
LICENSE_PAGE = "https://creativecommons.org/licenses/by-sa/4.0/legalcode"

# 既定の Python-urllib/x.y は弾く配布元がある（creativecommons.org は 403 を返す）。
USER_AGENT = "codegloss-convert-fugumt/0.1 (+https://github.com/shutx-net/codegloss)"

ATTRIBUTION = (
    "This model pack contains the weights of "
    f"{MODEL_ID} (FuguMT, by Satoru Takahashi / staka), "
    "licensed under CC-BY-SA-4.0, together with tokenizer files derived from "
    "the SentencePiece models published in the same repository. "
    "The derived files are adaptations and are distributed under the same "
    "licence. Source: https://huggingface.co/staka/fugumt-en-ja"
)

# SentencePiece のピース種別。3 = CONTROL, 4 = USER_DEFINED。
# どちらも「テキストとして再分割してはいけない」ので AddedToken にする。
PIECE_CONTROL = 3
PIECE_USER_DEFINED = 4

# 語彙に穴があったときに埋める点数。実在のピースは全て負の対数確率なので、
# これより低い点数は Viterbi で選ばれない。
HOLE_SCORE = -100.0


def fetch(url: str) -> bytes:
    """URL の中身を取ってくる。リダイレクトは urlopen が追う。"""
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request) as response:
        return response.read()


def download(destination: Path) -> None:
    """上流のファイルを destination に落とす。既にあるものは飛ばす。"""
    destination.mkdir(parents=True, exist_ok=True)
    for name in UPSTREAM_FILES:
        target = destination / name
        if target.exists():
            print(f"  {name}: すでにある")
            continue
        print(f"  {name}: 取得中")
        # resolve/main は CDN へ 307 で飛ぶ。
        target.write_bytes(fetch(f"{BASE_URL}/{name}"))


def build_tokenizer(spm_path: Path, vocab: dict[str, int]):
    """*.spm と vocab.json から HF の高速トークナイザを組み立てる。

    Marian の低速トークナイザは SentencePiece を「文字列をピースに割る」ためだけに
    使い、ピースから ID への対応は vocab.json で持つ。この 2 つは FuguMT では
    一致しているが、一致を仮定せず vocab.json 側の ID に合わせて並べ直す。

    手順は transformers の SpmConverter（4.46.3）と同じ。transformers 5 で
    Marian 用の変換が消えたので、必要な部分だけをここに写してある。
    """
    from sentencepiece import sentencepiece_model_pb2
    from tokenizers import AddedToken, Regex, Tokenizer, decoders, normalizers
    from tokenizers import pre_tokenizers
    from tokenizers.models import Unigram

    proto = sentencepiece_model_pb2.ModelProto()
    proto.ParseFromString(spm_path.read_bytes())

    if proto.trainer_spec.model_type != 1:
        raise SystemExit(f"{spm_path} is not a Unigram model")

    # vocab.json の ID の位置に (ピース, 点数) を置く。<pad> のように spm には
    # 無い語彙は穴のまま残るので、選ばれない点数で埋める。
    size = max(vocab.values()) + 1
    entries: list[tuple[str, float]] = [(token, HOLE_SCORE) for token, _ in
                                        sorted(vocab.items(), key=lambda pair: pair[1])]
    if len(entries) != size:
        raise SystemExit(f"vocab.json has holes: {len(entries)} tokens for {size} ids")
    for piece in proto.pieces:
        index = vocab.get(piece.piece)
        if index is None:
            raise SystemExit(f"{piece.piece!r} is in {spm_path.name} but not in vocab.json")
        entries[index] = (piece.piece, piece.score)

    unk_id = vocab["<unk>"]
    tokenizer = Tokenizer(Unigram(entries, unk_id=unk_id, byte_fallback=False))

    # 制御記号とユーザ定義記号は「これ以上分割しない語」として登録する。
    added = [
        AddedToken(piece.piece, normalized=False, special=piece.type == PIECE_CONTROL)
        for piece in proto.pieces
        if piece.type in (PIECE_CONTROL, PIECE_USER_DEFINED)
    ]
    for token in ("<pad>",):
        if token in vocab:
            added.append(AddedToken(token, normalized=False, special=True))
    tokenizer.add_tokens(added)

    charsmap = proto.normalizer_spec.precompiled_charsmap
    steps = [
        normalizers.Strip(left=False, right=True),
        normalizers.Replace(Regex(" {2,}"), "▁"),
    ]
    if charsmap:
        steps.insert(0, normalizers.Precompiled(charsmap))
    tokenizer.normalizer = normalizers.Sequence(steps)

    tokenizer.pre_tokenizer = pre_tokenizers.Metaspace(replacement="▁", prepend_scheme="always")
    tokenizer.decoder = decoders.Metaspace(replacement="▁", prepend_scheme="always")

    # post_processor は付けない。SpmConverter も付けず、candle の marian-mt の
    # 例は </s> を自分で足す。CandleTranslator も同じで、末尾に </s> が無ければ
    # 足す（あれば足さない）ので、どちらでも壊れない。
    return tokenizer


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def write_license(pack: Path) -> None:
    """CC-BY-SA-4.0 の全文を置く。取れなければ URL だけを書いて警告する。"""
    for url in LICENSE_URLS:
        try:
            text = fetch(url).decode("utf-8")
            break
        except OSError as error:  # ネットワークが無い環境でも変換自体は通す
            print(f"  警告: {url} から全文を取れなかった（{error}）", file=sys.stderr)
    else:
        print("  警告: LICENSE には URL だけを書く。配布前に全文を入れること", file=sys.stderr)
        text = (
            "The weights in this pack are licensed under "
            "Creative Commons Attribution-ShareAlike 4.0 International.\n"
            f"The full text is at {LICENSE_PAGE}\n"
        )
    (pack / "LICENSE").write_text(text, encoding="utf-8")


def verify(pack: Path, source_dir: Path) -> int:
    """生成した高速トークナイザを低速トークナイザと突き合わせる。

    transformers<5 と sentencepiece が要る。無ければ黙って飛ばす。
    """
    try:
        from transformers import MarianTokenizer
    except ImportError:
        print("  transformers が無いので照合を飛ばす", file=sys.stderr)
        return 0

    from tokenizers import Tokenizer

    slow = MarianTokenizer.from_pretrained(str(source_dir))
    fast = Tokenizer.from_file(str(pack / "tokenizer-source.json"))

    samples = [
        "Returns `UserDetails` when authentication succeeds.",
        "Calls X0Q before X1Q.",
        "X0Q the id to look up",
        "See https://example.com/docs/auth for the protocol.",
        "TODO: drop the cache.",
        "A sentence  with  doubled   spaces.",
        "Naïve café — em dash and accents.",
    ]
    mismatches = 0
    for sample in samples:
        expected = slow(sample)["input_ids"]
        # 低速側は末尾に </s> を足す。高速側は足さないので落として比べる。
        expected = expected[:-1] if expected and expected[-1] == slow.eos_token_id else expected
        actual = fast.encode(sample, add_special_tokens=True).ids
        if expected != actual:
            mismatches += 1
            print(f"  不一致: {sample!r}\n    低速 {expected}\n    高速 {actual}", file=sys.stderr)
    print(f"  照合: {len(samples) - mismatches}/{len(samples)} 一致")
    return mismatches


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out", type=Path, help="モデルパックの出力先")
    parser.add_argument(
        "--from-dir",
        type=Path,
        help="上流ファイルの置き場。省略すると Hugging Face から取得して out/.upstream に置く",
    )
    parser.add_argument("--verify", action="store_true", help="低速トークナイザと突き合わせる")
    arguments = parser.parse_args()

    pack: Path = arguments.out
    pack.mkdir(parents=True, exist_ok=True)

    source_dir = arguments.from_dir
    if source_dir is None:
        source_dir = pack / ".upstream"
        print(f"上流ファイルを {source_dir} に取得する")
        download(source_dir)

    missing = [name for name in UPSTREAM_FILES if not (source_dir / name).is_file()]
    if missing:
        raise SystemExit(f"{source_dir} に {', '.join(missing)} が無い")

    vocab = json.loads((source_dir / "vocab.json").read_text(encoding="utf-8"))

    print("トークナイザを生成する")
    for spm, name in (("source.spm", "tokenizer-source.json"),
                      ("target.spm", "tokenizer-target.json")):
        tokenizer = build_tokenizer(source_dir / spm, vocab)
        tokenizer.save(str(pack / name))
        print(f"  {name}")

    print("重みと設定を置く")
    # 重みは変換しない。candle の VarBuilder::from_pth が pickle を直接読むので、
    # safetensors 化のためだけに torch を入れる意味が無い。README を参照。
    for name in ("config.json", "generation_config.json", "pytorch_model.bin"):
        shutil.copyfile(source_dir / name, pack / name)
        print(f"  {name}")

    write_license(pack)
    (pack / "NOTICE").write_text(ATTRIBUTION + "\n", encoding="utf-8")

    weights_digest = sha256(pack / "pytorch_model.bin")
    files = {}
    for path in sorted(pack.iterdir()):
        if path.is_file() and path.name != "manifest.json":
            files[path.name] = {"sha256": sha256(path), "bytes": path.stat().st_size}

    manifest = {
        "model_id": MODEL_ID,
        # 重みが変われば必ず変わる。これが翻訳キャッシュのキーの一部になる。
        "model_version": f"fugumt-en-ja-{weights_digest[:12]}",
        "license": "CC-BY-SA-4.0",
        "attribution": ATTRIBUTION,
        "source_language": "en",
        "target_language": "ja",
        "files": files,
    }
    (pack / "manifest.json").write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(f"  manifest.json（model_version = {manifest['model_version']}）")

    mismatches = verify(pack, source_dir) if arguments.verify else 0
    print(f"完成: {pack}")
    return 1 if mismatches else 0


if __name__ == "__main__":
    sys.exit(main())
