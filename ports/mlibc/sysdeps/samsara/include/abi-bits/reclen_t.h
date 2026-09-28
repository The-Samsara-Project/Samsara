#ifndef _ABIBITS_RECLEN_T_H
#define _ABIBITS_RECLEN_T_H

#include <mlibc-config.h>

/* `d_reclen`: the on-disk length of one directory entry, as a `size_t` on
 * every ABI Linux supports. It exists so a `readdir` buffer walk can advance
 * without recomputing the entry's length. */
typedef size_t reclen_t;

#endif
