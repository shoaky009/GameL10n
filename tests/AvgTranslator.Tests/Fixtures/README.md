# Engine archive reference fixture

`engine-archives-e9b8018.zip` contains only `python/__init__.py`, `python/archives/__init__.py`, `rpa.py` and `xp3.py` from [GalTransl-GalGameReverse-Skill](https://github.com/GalTransl/GalTransl-GalGameReverse-Skill/tree/e9b801879f37cee98a9dfc8afbbcf5ffa929c04c/python/archives), commit `e9b801879f37cee98a9dfc8afbbcf5ffa929c04c`. The archive modules retain their MIT copyright and license headers. It is an offline test fixture, not a production dependency or a substitute for the application's pinned library.

Archive component integration tests require Python 3.10+ for the test process. Set `AVG_TEST_PYTHON` to its executable, or provide `python3`/`python` on PATH; tests also recognize the application's bundled Python. Production components continue to require the shared, receipt-checked portable interpreter and never select system Python.
