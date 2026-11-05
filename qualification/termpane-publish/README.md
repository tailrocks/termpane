# No-publication release rehearsal

This workflow is a provisional rehearsal for the approved 0.1.1 source. It
uses the checked-in draft recovery configuration, a reviewed Velnor candidate
PR and its generic test-only preview and upload-config renderers. The preview
manifest contains placeholder artifact hashes: it is not an official Velnor
release manifest or publication authority. The draft configuration selects
no additional publication features.

The generated `release-rehearsal.yml` workflow dispatches the consumer-owned
`rehearse.sh` from reviewed `main` without publication credentials. The wrapper
checks the public same-repository PR head, main rules, and CI provenance before
checkout, before candidate execution, and again after the rehearsal. It fails
closed on incomplete or ambiguous public evidence.

An authenticated, independent verifier must review all PR reviews, comments,
replies and thread-resolution state at the exact candidate head, and establish
that all required checks and reviews pass. This is mandatory before using the
rehearsal as qualification: the credential-free wrapper cannot verify thread
resolution or decide whether every comment is addressed. Do not bypass the
environment reviewer.

The native coordinator fixture uses source commit
`f1c2dfde45bdbe91425af6bb17d80e606703baac`, the draft intent's package,
version and features, and the pinned Linux release-plz and Rust tooling. It
substitutes the historical, signed 0.1.73 generator manifest only for its
offline attestation fixture. The existing Velnor Rust verifier checks the
installed toolchain against the pinned channel and component hashes immediately
after installation, before the fixture replaces Cargo with its publication
wrapper. The coordinator runs in Docker with networking disabled, fake
GitHub, registry and OIDC endpoints, and an intercepted Cargo publication.
The workflow compares every packaged file with the approved Git commit,
including resolved tracked symlinks. It also compares generated Cargo metadata
with a fresh package and checks `EraseDisplayPolicy` and the embedded source
SHA. This rehearsal does not prove a real GitHub OIDC exchange, registry
upload, or published artifact identity. The test-only dispatcher does not
remove the production guards.

After the offline coordinator succeeds, the wrapper verifies the fixture's
bounded observation against the actual candidate and source Git commits and
trees, both intent files, the fixture package, and the pinned generator
manifest. It compares the complete package with the approved source before
printing `TERMPANE_FIXTURE_OBSERVATION=` followed by bounded JSON. This marker
is an untrusted result produced by reviewed rehearsal code, not a registry
publication, an attestation, or release authorization. A separate trusted
verifier must authenticate the exact successful workflow run and executor job,
verify the inputs and review state independently, and reject missing, duplicate,
stale, malformed, or inconsistent markers.

When the generated runner enables its output handoff, the wrapper writes a
bounded untrusted tar containing exactly `receipt.json`, `fixture-evidence.json`,
`supplied-intent.toml`, `effective-intent.toml`, `generator-manifest.json`, and
`termpane-0.1.1.crate`. It includes no credentials or logs. The trusted verifier
must independently check the bundle contents and their hashes; artifact custody
proves only which bytes the rehearsal supplied.

After the authorized Velnor release exists, regenerate and review the actual
termpane release intent and workflow with the published generator. Compare the
actual intent with this provisional draft, then update and rerun the reviewed
rehearsal against that actual intent before publishing. Independently compare
the downloaded published crate and its complete contents with the approved
source. Neither this rehearsal nor a successful dry run authorizes publishing
or removal of Velnor's production guards.
