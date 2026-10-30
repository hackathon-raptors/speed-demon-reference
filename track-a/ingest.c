/*
 * Speed Demon 2026 Track A (Ingest), optimized entry in C.
 *
 * Byte-level parse of requests.log over an mmap, grouping by (UTC hour, endpoint)
 * in a single open-addressed hash table, nearest-rank percentiles, bytewise output
 * order. No strptime: the hour key is just the first 13 bytes of the timestamp and
 * a 5xx status is "first digit is '5'". Standard library only (D = 1).
 *
 * spec: specs/a-ingest.md. Output must match the reference's summary.txt byte for byte.
 */
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#define HOURLEN 13          /* "YYYY-MM-DDTHH" */
#define TSLEN 27            /* full timestamp, fixed width */

typedef struct {
    uint32_t key_off;       /* offset of (hour13 + endpoint) in the key arena */
    uint32_t key_len;       /* 13 + endpoint length */
    uint32_t *lat;          /* latencies in input order */
    uint32_t count;
    uint32_t cap;
    uint32_t errors;
} Group;

static char *arena;                  /* packed group keys */
static size_t arena_len, arena_cap;

static Group *groups;                /* dense array of groups */
static uint32_t ngroups, groups_cap;

static int32_t *table;               /* hash slots -> group index, or -1 */
static size_t table_mask;

static void die(const char *msg) { perror(msg); exit(1); }

static uint64_t fnv1a(const char *p, size_t n) {
    uint64_t h = 1469598103934665603ULL;
    for (size_t i = 0; i < n; i++) { h ^= (unsigned char)p[i]; h *= 1099511628211ULL; }
    return h;
}

static size_t arena_put(const char *hour, const char *ep, size_t eplen) {
    size_t need = arena_len + HOURLEN + eplen;
    if (need > arena_cap) {
        while (need > arena_cap) arena_cap = arena_cap ? arena_cap * 2 : (1 << 20);
        arena = realloc(arena, arena_cap);
        if (!arena) die("realloc arena");
    }
    size_t off = arena_len;
    memcpy(arena + off, hour, HOURLEN);
    memcpy(arena + off + HOURLEN, ep, eplen);
    arena_len = need;
    return off;
}

static void table_grow(void) {
    size_t newsize = (table_mask + 1) * 2;
    if (newsize < (1 << 16)) newsize = 1 << 16;
    int32_t *nt = malloc(newsize * sizeof(int32_t));
    if (!nt) die("malloc table");
    for (size_t i = 0; i < newsize; i++) nt[i] = -1;
    size_t nmask = newsize - 1;
    for (uint32_t g = 0; g < ngroups; g++) {
        const char *k = arena + groups[g].key_off;
        size_t slot = fnv1a(k, groups[g].key_len) & nmask;
        while (nt[slot] != -1) slot = (slot + 1) & nmask;
        nt[slot] = (int32_t)g;
    }
    free(table);
    table = nt;
    table_mask = nmask;
}

/* Return the group for (hour13, endpoint), creating it on first sight. */
static Group *group_for(const char *hour, const char *ep, size_t eplen) {
    char keybuf[HOURLEN + 128];
    memcpy(keybuf, hour, HOURLEN);
    memcpy(keybuf + HOURLEN, ep, eplen);
    size_t klen = HOURLEN + eplen;
    size_t slot = fnv1a(keybuf, klen) & table_mask;
    for (;;) {
        int32_t gi = table[slot];
        if (gi == -1) break;
        Group *g = &groups[gi];
        if (g->key_len == klen && memcmp(arena + g->key_off, keybuf, klen) == 0) return g;
        slot = (slot + 1) & table_mask;
    }
    if (ngroups + 1 > groups_cap) {
        groups_cap = groups_cap ? groups_cap * 2 : (1 << 16);
        groups = realloc(groups, groups_cap * sizeof(Group));
        if (!groups) die("realloc groups");
    }
    /* arena_put may move the arena; recompute nothing else points into it mid-parse. */
    uint32_t idx = ngroups++;
    size_t off = arena_put(hour, ep, eplen);
    groups[idx].key_off = (uint32_t)off;
    groups[idx].key_len = (uint32_t)klen;
    groups[idx].lat = NULL;
    groups[idx].count = 0;
    groups[idx].cap = 0;
    groups[idx].errors = 0;
    table[slot] = (int32_t)idx;
    if ((ngroups) * 10 >= (table_mask + 1) * 7) table_grow();  /* keep load < 0.7 */
    return &groups[idx];
}

static int cmp_u32(const void *a, const void *b) {
    uint32_t x = *(const uint32_t *)a, y = *(const uint32_t *)b;
    return (x > y) - (x < y);
}

static int cmp_group(const void *a, const void *b) {
    const Group *g = &groups[*(const uint32_t *)a], *h = &groups[*(const uint32_t *)b];
    size_t n = g->key_len < h->key_len ? g->key_len : h->key_len;
    int c = memcmp(arena + g->key_off, arena + h->key_off, n);
    if (c) return c;
    return (g->key_len > h->key_len) - (g->key_len < h->key_len);
}

/* append u (decimal, no leading zeros) to out, returning the new length */
static size_t put_u64(char *out, size_t len, uint64_t u) {
    char tmp[20];
    int n = 0;
    if (u == 0) tmp[n++] = '0';
    while (u) { tmp[n++] = (char)('0' + u % 10); u /= 10; }
    while (n) out[len++] = tmp[--n];
    return len;
}

int main(int argc, char **argv) {
    const char *data = "/data", *out = "/out";
    for (int i = 1; i + 1 < argc; i++) {
        if (!strcmp(argv[i], "--data")) data = argv[++i];
        else if (!strcmp(argv[i], "--out")) out = argv[++i];
    }

    char inpath[4096], outpath[4096];
    snprintf(inpath, sizeof inpath, "%s/requests.log", data);
    snprintf(outpath, sizeof outpath, "%s/summary.txt", out);

    int fd = open(inpath, O_RDONLY);
    if (fd < 0) die("open requests.log");
    struct stat st;
    if (fstat(fd, &st) < 0) die("fstat");
    size_t sz = (size_t)st.st_size;
    const char *base = "";
    if (sz) {
        base = mmap(NULL, sz, PROT_READ, MAP_PRIVATE, fd, 0);
        if (base == MAP_FAILED) die("mmap");
        madvise((void *)base, sz, MADV_SEQUENTIAL);
    }
    close(fd);

    table_mask = (1 << 16) - 1;
    table = malloc((table_mask + 1) * sizeof(int32_t));
    if (!table) die("malloc table");
    for (size_t i = 0; i <= table_mask; i++) table[i] = -1;

    const char *p = base, *end = base + sz;
    while (p < end) {
        const char *line = p;
        const char *nl = memchr(p, '\n', (size_t)(end - p));
        if (!nl) break;
        p = nl + 1;
        /* line = ts(27) SP method SP endpoint SP status SP latency */
        const char *hour = line;                 /* first 13 bytes */
        const char *q = line + TSLEN + 1;         /* start of method */
        const char *ms = memchr(q, ' ', (size_t)(nl - q));  /* end of method */
        const char *ep = ms + 1;
        const char *es = memchr(ep, ' ', (size_t)(nl - ep)); /* end of endpoint */
        size_t eplen = (size_t)(es - ep);
        const char *status = es + 1;              /* 3 digits */
        const char *latp = status + 4;            /* status(3) SP -> latency */

        Group *g = group_for(hour, ep, eplen);
        if (g->count == g->cap) {
            g->cap = g->cap ? g->cap * 2 : 4;
            g->lat = realloc(g->lat, g->cap * sizeof(uint32_t));
            if (!g->lat) die("realloc lat");
        }
        uint32_t v = 0;
        for (const char *d = latp; d < nl; d++) v = v * 10u + (uint32_t)(*d - '0');
        g->lat[g->count++] = v;
        if (status[0] == '5') g->errors++;        /* 500..599 */
    }

    uint32_t *order = malloc(ngroups * sizeof(uint32_t));
    if (!order && ngroups) die("malloc order");
    for (uint32_t i = 0; i < ngroups; i++) order[i] = i;
    qsort(order, ngroups, sizeof(uint32_t), cmp_group);

    size_t obuf_cap = 1 << 20, obuf_len = 0;
    char *obuf = malloc(obuf_cap);
    if (!obuf) die("malloc obuf");

    for (uint32_t i = 0; i < ngroups; i++) {
        Group *g = &groups[order[i]];
        qsort(g->lat, g->count, sizeof(uint32_t), cmp_u32);
        uint64_t n = g->count;
        uint32_t p50 = g->lat[(50 * n + 99) / 100 - 1];
        uint32_t p95 = g->lat[(95 * n + 99) / 100 - 1];
        uint32_t p99 = g->lat[(99 * n + 99) / 100 - 1];
        uint64_t ppm = (uint64_t)g->errors * 1000000ULL / n;

        if (obuf_len + g->key_len + 160 > obuf_cap) {
            while (obuf_len + g->key_len + 160 > obuf_cap) obuf_cap *= 2;
            obuf = realloc(obuf, obuf_cap);
            if (!obuf) die("realloc obuf");
        }
        const char *k = arena + g->key_off;
        memcpy(obuf + obuf_len, k, HOURLEN); obuf_len += HOURLEN;      /* YYYY-MM-DDTHH */
        memcpy(obuf + obuf_len, ":00:00Z", 7); obuf_len += 7;
        obuf[obuf_len++] = ' ';
        memcpy(obuf + obuf_len, k + HOURLEN, g->key_len - HOURLEN); obuf_len += g->key_len - HOURLEN;
        obuf[obuf_len++] = ' ';
        obuf_len = put_u64(obuf, obuf_len, n);        obuf[obuf_len++] = ' ';
        obuf_len = put_u64(obuf, obuf_len, p50);      obuf[obuf_len++] = ' ';
        obuf_len = put_u64(obuf, obuf_len, p95);      obuf[obuf_len++] = ' ';
        obuf_len = put_u64(obuf, obuf_len, p99);      obuf[obuf_len++] = ' ';
        obuf_len = put_u64(obuf, obuf_len, g->errors); obuf[obuf_len++] = ' ';
        obuf_len = put_u64(obuf, obuf_len, ppm);      obuf[obuf_len++] = '\n';
    }

    int ofd = open(outpath, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (ofd < 0) die("open summary.txt");
    size_t off = 0;
    while (off < obuf_len) {
        ssize_t w = write(ofd, obuf + off, obuf_len - off);
        if (w < 0) die("write");
        off += (size_t)w;
    }
    if (close(ofd) < 0) die("close");
    return 0;
}
