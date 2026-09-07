# Model assets

What gets uploaded to S3 so `kobold-tts` can fetch its model without a Hugging
Face token.

The upstream repository, [kyutai/pocket-tts][repo], is *gated*: downloading it
means accepting terms and presenting a token. That is a reasonable thing to ask
of someone building kobold and an unreasonable thing to ask of someone running
it. The model is CC-BY-4.0, which permits redistribution with attribution, so
the fix is to mirror it once and serve it openly. The attribution obligation
that comes with that is `pocket-tts/v1/NOTICE`, and it is uploaded alongside
the weights rather than left behind in this repo.

[repo]: https://huggingface.co/kyutai/pocket-tts

## Layout

```
pocket-tts/v1/                       everything here is uploaded verbatim
  NOTICE                             attribution and acceptable use
  SHA256SUMS                         checksum per file
  tokenizer.model                    58 KB
  tts_b6369a24.safetensors           225 MB
  embeddings/<voice>.safetensors     500 KB each
```

The paths under `v1/` are exactly the paths the engine already requests from
Hugging Face, so pointing it at the mirror is a change of base URL and nothing
else.

`v1` is a version, not decoration. Objects are uploaded immutable with a
one-year `Cache-Control`, which is only safe because new weights become `v2`
instead of replacing what clients have cached.

## The large files are not in git

`.gitignore` excludes the weights, the tokenizer and the embeddings. 225 MB of
model does not belong in a repository whose entire binary is 2.7 MB, and it
would be pulled by everyone who clones regardless of whether they ever build
the engine. `NOTICE`, `SHA256SUMS` and these scripts *are* tracked, so the
bundle's shape and provenance are reviewable even though its payload is not.

A fresh clone therefore has an empty bundle. Repopulate it from a machine that
has run the engine at least once:

```bash
S=$(ls -d ~/.cache/huggingface/hub/models--kyutai--pocket-tts/snapshots/* | head -1)
cp -L "$S"/tts_b6369a24.safetensors "$S"/tokenizer.model assets/pocket-tts/v1/
cp -L "$S"/embeddings/*.safetensors assets/pocket-tts/v1/embeddings/
```

then `sha256sum -c SHA256SUMS` from inside `pocket-tts/v1` to confirm the copy
matches what was published.

## Publishing

```bash
HF_TOKEN=hf_... ./fetch-voices.sh      # once, to fill in missing voices
./upload.sh my-bucket                  # sync to s3://my-bucket/pocket-tts/v1
./upload.sh --verify my-bucket         # fetch anonymously, as a user would
```

`upload.sh` verifies checksums before sending anything and prints the bucket
policy needed to make the objects publicly readable. `--verify` is the step
worth not skipping: an upload can succeed while the objects stay private, which
looks perfectly fine from an authenticated shell and fails on every user's
machine.

## Voices

All eight are present and match `VOICES` in `src/tts.rs`: `alba`, `marius`,
`javert`, `jean`, `fantine`, `cosette`, `eponine`, `azelma`. A voice named
there and absent here is a runtime 404 for whoever selects it, so the two lists
have to move together.

Each holds one `audio_prompt` tensor of shape `[1, N, 1024]` in F32, where `N`
is the length of the reference audio and differs per voice, between 125 and
161. Files therefore vary from 500 KB to 645 KB; equal sizes across every voice
would be a sign something had been copied rather than fetched.

## Compression

Not used. The weights gzip to about 80% of their size, so a fifth off a 225 MB
download in exchange for a decompression step and no range requests. If
bandwidth ever justifies it, `Content-Encoding: gzip` on the object is the
place to do it, since the transfer decodes transparently and the engine keeps
reading a plain file.
