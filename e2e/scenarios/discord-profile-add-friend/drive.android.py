import pathlib, runpy, sys
sys.argv = [str(pathlib.Path(__file__).with_name('drive.py')), 'android']
runpy.run_path(sys.argv[0], run_name='__main__')
