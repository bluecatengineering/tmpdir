# TmpDir

Useful to create temp directories and copying their contents on completion
of some action. Tmp dirs will be created using [`env::temp_dir`] with
some random characters prefixed to prevent a name clash

`copy` will traverse recursively through a directory and copy all file
contents to some destination dir. It will not follow symlinks.

## Example

```rust
use tmpdir::TmpDir;
use tokio::{fs, io::AsyncWriteExt};

let tmp = TmpDir::new("foo").await.unwrap();
let new_tmp = TmpDir::new("bar").await.unwrap();

new_tmp.copy(tmp.as_ref()).await;
new_tmp.close().await; // not necessary to explicitly call
```
