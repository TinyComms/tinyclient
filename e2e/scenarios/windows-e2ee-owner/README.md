# Windows E2EE private_dir owner access

Honest **component Windows permission proof** for chatd's E2EE text folder ACL.
It is not all-platform native GUI certification and does not post a full `e2e-proof`
plan by itself (`release: false`).

## What it proves

On Windows, `private_dir` must grant a protected DACL to the **current process
token user** and SYSTEM. Granting owner-rights (`OW`) alone locks out the
interactive user when the directory owner is `BUILTIN\Administrators` (elevated
staging, restore, or copy) even though preexisting encrypted `device.state` /
cache handles can still be open.

## Driver

`drive.windows.ps1` uses the lab's `E2E_SRC_DIR` source checkout (or an explicit
`TINYCLIENT_SOURCE` override). It requires a limited, non-system token and
`TINYCLIENT_WINDOWS_E2EE_OWNER_FIXTURE` pointing at an Administrators-owned test
directory with the dedicated eight-byte `marker.bin` value `01 02 03 04 05 09 09 09`.
The directory must initially grant that user full control and contain synthetic
`device.state` and `cache-c1.log` files; this fixture test overwrites its synthetic
state. Never point it at a real profile. Missing prerequisites, zero selected tests
or an ignored test that did not run fail the driver. Ordinary CI coverage runs
`the_folder_is_the_owners_alone` without that fixture.

Already-locked product folders are left unchanged: a limited token without
`WRITE_DAC` cannot heal them, and this scenario does not take ownership or
delete history.
