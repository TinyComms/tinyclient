# OpenH264 download

`TINYCLIENT_PROVE=openh264-download` starts the shipped `mediad` with a codec folder of its
own that is empty (an isolated `LOCALAPPDATA` on Windows, `HOME` elsewhere), and nothing is
faked: it fetches Cisco's OpenH264 2.6.0 from `ciscobinary.openh264.org` the way a call does,
with three tries of 20 seconds each, and it exits 0 only when the library loads. The loader
takes Cisco's hash alone, so a file that is not Cisco's is no proof.

The driver holds the step only when mediad says `openh264-download: proved` and the library
is on disk in that folder. The exit code alone proves nothing: any mediad that is asked for no
call exits 0.

On the base the scenario fails. A build from before the change ignores the variable, exits 0 at
once and says nothing, and the folder stays empty, which is the defect (#967): the download's
request had no TLS backend, so `%LOCALAPPDATA%\tinyclient` was never filled.

It needs the network: the lab host must reach `ciscobinary.openh264.org` over HTTPS. Without
it the step fails after about 65 seconds at most, with the words a person is shown. The precise
reason is then in `logs/mediad.log` under the run's isolated data folder, next to the driver's
own log.

`openh264-download-retry` is its offline sibling: the same download's tries, pauses and words,
from the unit tests, with no network. This one is the real download.
