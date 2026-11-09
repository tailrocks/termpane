# Local work preservation

This branch preserves unfinished and historical work, not an integrated release. Its archive merge retains the original commit histories as parents without applying their changes to main. The inventory maps original branch names, detached worktrees, recovered commits, and exact saved-file hashes.

The release goal remains paused. Historical audit files and saved runbooks are evidence, not current policy or release authorization. Build caches are excluded. No release tags or published artifacts are changed.

Orphan Git objects are stored as base64 with original object IDs and byte hashes in the inventory, preserving even unfinished conflict-resolution content without placing conflict markers in active source.
