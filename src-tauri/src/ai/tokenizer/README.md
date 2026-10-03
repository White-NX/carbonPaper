# DeepSeek V3 request estimator

`deepseek-v3.json.gz` is the gzip-compressed `src/lib/deepseek-tokenizer/tokenizer.json`
from the local Daydream project, which uses the DeepSeek V3 vocabulary distributed
with `deepseek_tokenizer`. The upstream code/tokenizer repository is
https://huggingface.co/deepseek-ai/DeepSeek-V3 (see `LICENSE-CODE`).

CarbonPaper loads it once through native Rust `tokenizers`; no Python, daemon,
network request, or runtime download is involved. The uncompressed vocabulary is
7,847,602 bytes. Request estimates use DeepSeek token count × 1.3 (rounded up),
plus 256 tokens of protocol overhead, for every provider. Provider-reported usage
still settles reservations. This coefficient is a heuristic, not a universal
upper bound across models; it must not be displayed as actual billed usage.
