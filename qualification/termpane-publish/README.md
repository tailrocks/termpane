# No-publication release rehearsal

This workflow is a provisional rehearsal for the approved 0.1.1 source. It
uses the checked-in draft recovery configuration, a reviewed Velnor candidate
PR and its generic test-only preview and upload-config renderers. The preview
manifest contains placeholder artifact hashes: it is not an official Velnor
release manifest or publication authority. The draft configuration selects
no additional publication features.

The `release-rehearsal.yml` workflow is hand-owned and explicitly preserved
by `.velnor/config.toml`. Its protected job runs only on a manual dispatch
from `main` in the existing `release` environment. It requests no GitHub
permissions or real registry/OIDC credential. Before approving the job, review
the exact candidate PR head, all its reviews, comments and replies, and its
required checks. The job independently checks the same-repository PR head,
main rules and CI provenance before checkout and again before candidate code
runs. It uses unauthenticated GitHub reads and fails closed on incomplete or
ambiguous evidence. Do not bypass the environment reviewer.

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

After the authorized Velnor release exists, regenerate and review the actual
termpane release intent and workflow with the published generator. Compare the
actual intent with this provisional draft, then update and rerun the reviewed
rehearsal against that actual intent before publishing. Independently compare
the downloaded published crate and its complete contents with the approved
source. Neither this rehearsal nor a successful dry run authorizes publishing
or removal of Velnor's production guards.
