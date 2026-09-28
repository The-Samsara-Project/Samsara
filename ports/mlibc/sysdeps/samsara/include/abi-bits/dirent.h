#ifndef _ABIBITS_DIRENT_H
#define _ABIBITS_DIRENT_H

#include <abi-bits/ino_t.h>
#include <bits/off_t.h>
#include <bits/reclen_t.h>

/* Entry types, matching the `d_type` values Linux's getdents64 reports. */
#define DT_UNKNOWN 0
#define DT_FIFO 1
#define DT_CHR 2
#define DT_DIR 4
#define DT_BLK 6
#define DT_REG 8
#define DT_LNK 10
#define DT_SOCK 12
#define DT_WHT 14

#endif
