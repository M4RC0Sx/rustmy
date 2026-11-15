"""Protocol and error constants, under the module names other MySQL drivers
use (``from rustmy.constants import ER``), from the MySQL and MariaDB
documentation.
"""

from rustmy.constants import CLIENT
from rustmy.constants import CR
from rustmy.constants import ER
from rustmy.constants import FIELD_TYPE
from rustmy.constants import FLAG
from rustmy.constants import SERVER_STATUS


__all__ = ["CLIENT", "CR", "ER", "FIELD_TYPE", "FLAG", "SERVER_STATUS"]
