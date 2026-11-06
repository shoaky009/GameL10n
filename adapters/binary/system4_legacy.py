"""AIN v14 MSG1/FNL variant with the original bundled Noto glyph extension."""
from binary_common import main
from system4 import handle

if __name__ == '__main__':
    main(lambda request: handle(request, legacy_font=True))
