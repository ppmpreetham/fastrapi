from .fastrapi import *

__doc__ = fastrapi.__doc__
__version__ = fastrapi.__version__
if hasattr(fastrapi, "__all__"):
    __all__ = list(dict.fromkeys(fastrapi.__all__))


def __getattr__(name):
    # The extension imports some attributes on first use, so the star import cannot copy them.
    return getattr(fastrapi, name)
