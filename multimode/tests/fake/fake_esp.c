/* Fake ESP-IDF layer for the PC tests of mode_switch.c and settings_store.c.
 * Only what those two files call; behaviour follows the real APIs closely
 * enough for the properties under test (which partition an NVS call touches,
 * what is written, what survives). */
#include "fake_esp.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "bootloader_random.h"
#include "esp_app_desc.h"
#include "esp_image_format.h"
#include "esp_ota_ops.h"
#include "esp_random.h"
#include "esp_system.h"
#include "nvs_flash.h"

int fake_log_warnings, fake_critical_depth, fake_critical_enters;
jmp_buf fake_restart_jmp;
int fake_restarts;
esp_err_t fake_verify_err, fake_set_boot_err;
int fake_verify_calls, fake_set_boot_calls;
uint32_t fake_random_next;

void esp_restart(void)
{
    fake_restarts++;
    longjmp(fake_restart_jmp, 1);
}

const char *esp_err_to_name(esp_err_t e)
{
    static char buf[8][24];
    static int i;
    char *b = buf[i++ & 7];
    snprintf(b, 24, "ERR_0x%x", (unsigned)e);
    return b;
}

void bootloader_random_enable(void) {}
void bootloader_random_disable(void) {}
uint32_t esp_random(void)
{
    return fake_random_next++ * 2654435761u;
}

/* ---- partitions ---------------------------------------------------------- */

#define MAX_PARTS 16
static esp_partition_t s_parts[MAX_PARTS];
static int s_nparts;
static const esp_partition_t *s_running, *s_boot;

esp_partition_t *fake_part_add(esp_partition_type_t type, esp_partition_subtype_t subtype, const char *label,
                               uint32_t address, uint32_t size)
{
    esp_partition_t *p = &s_parts[s_nparts++];
    memset(p, 0, sizeof(*p));
    p->type = type;
    p->subtype = subtype;
    p->address = address;
    p->size = size;
    snprintf(p->label, sizeof(p->label), "%s", label);
    return p;
}

void fake_part_make_image(esp_partition_t *p)
{
    memset(p->head, 0, sizeof(p->head));
    esp_image_header_t h;
    memset(&h, 0, sizeof(h));
    h.magic = ESP_IMAGE_HEADER_MAGIC;
    h.segment_count = 3;
    h.chip_id = ESP_CHIP_ID_ESP32S3;
    memcpy(p->head, &h, sizeof(h));
    uint32_t desc = ESP_APP_DESC_MAGIC_WORD;
    memcpy(p->head + sizeof(esp_image_header_t) + sizeof(esp_image_segment_header_t), &desc, 4);
}

void fake_set_running(const esp_partition_t *p) { s_running = p; }
void fake_set_boot(const esp_partition_t *p) { s_boot = p; }
const esp_partition_t *fake_boot(void) { return s_boot; }

const esp_partition_t *esp_partition_find_first(esp_partition_type_t type, esp_partition_subtype_t subtype,
                                                const char *label)
{
    for (int i = 0; i < s_nparts; i++) {
        if (s_parts[i].type == type && s_parts[i].subtype == subtype &&
            (label == NULL || strcmp(s_parts[i].label, label) == 0)) {
            return &s_parts[i];
        }
    }
    return NULL;
}

esp_err_t esp_partition_read(const esp_partition_t *p, size_t offset, void *dst, size_t size)
{
    if (offset + size > sizeof(p->head)) {
        memset(dst, 0, size);
        return ESP_OK; /* beyond the fake header bytes: erased-looking zeros */
    }
    memcpy(dst, p->head + offset, size);
    return ESP_OK;
}

const esp_partition_t *esp_ota_get_running_partition(void) { return s_running; }
const esp_partition_t *esp_ota_get_boot_partition(void) { return s_boot; }

esp_err_t esp_ota_set_boot_partition(const esp_partition_t *p)
{
    fake_set_boot_calls++;
    if (fake_set_boot_err != ESP_OK) {
        return fake_set_boot_err;
    }
    s_boot = p;
    return ESP_OK;
}

esp_err_t esp_image_verify(esp_image_load_mode_t mode, const esp_partition_pos_t *part, esp_image_metadata_t *data)
{
    (void)mode;
    (void)part;
    (void)data;
    fake_verify_calls++;
    return fake_verify_err;
}

/* ---- NVS ----------------------------------------------------------------- */

#define MAX_NPARTS 6
#define MAX_ENTRIES 64
#define MAX_HANDLES 8
#define MAX_LOG 512
#define BLOB_CAP 1200

typedef struct {
    char name[20];
    bool exists, inited;
    esp_err_t init_err;
} npart_t;

typedef struct {
    bool used;
    int part;
    char ns[16], key[16];
    bool is_u8;
    uint8_t data[BLOB_CAP];
    size_t len;
} entry_t;

typedef struct {
    bool used, ro;
    int part;
    char ns[16];
} handle_t;

typedef struct {
    fake_nvs_op_t op;
    char part[20], ns[16];
} logrec_t;

static npart_t s_np[MAX_NPARTS];
static entry_t s_ent[MAX_ENTRIES];
static handle_t s_h[MAX_HANDLES];
static logrec_t s_log[MAX_LOG];
static int s_nlog;
/* A namespace exists once opened read-write, or once it holds a key. */
static struct { bool used; int part; char ns[16]; } s_ns[16];

void fake_reset_all(void)
{
    s_nparts = 0;
    s_running = s_boot = NULL;
    memset(s_np, 0, sizeof(s_np));
    memset(s_ent, 0, sizeof(s_ent));
    memset(s_h, 0, sizeof(s_h));
    memset(s_ns, 0, sizeof(s_ns));
    s_nlog = 0;
    fake_log_warnings = fake_critical_depth = fake_critical_enters = fake_restarts = 0;
    fake_verify_err = fake_set_boot_err = ESP_OK;
    fake_verify_calls = fake_set_boot_calls = 0;
    fake_random_next = 1;
}

static void logop(fake_nvs_op_t op, const char *part, const char *ns)
{
    if (s_nlog < MAX_LOG) {
        logrec_t *r = &s_log[s_nlog++];
        r->op = op;
        snprintf(r->part, sizeof(r->part), "%s", part ? part : "");
        snprintf(r->ns, sizeof(r->ns), "%s", ns ? ns : "");
    }
}

static int np_find(const char *name)
{
    for (int i = 0; i < MAX_NPARTS; i++) {
        if (s_np[i].exists && strcmp(s_np[i].name, name) == 0) {
            return i;
        }
    }
    return -1;
}

void fake_nvs_part_add(const char *name)
{
    for (int i = 0; i < MAX_NPARTS; i++) {
        if (!s_np[i].exists) {
            s_np[i].exists = true;
            snprintf(s_np[i].name, sizeof(s_np[i].name), "%s", name);
            return;
        }
    }
    abort();
}

void fake_nvs_part_fail_init(const char *name, esp_err_t err)
{
    int p = np_find(name);
    if (p < 0) {
        abort();
    }
    s_np[p].init_err = err;
}

static bool ns_exists(int part, const char *ns)
{
    for (int i = 0; i < 16; i++) {
        if (s_ns[i].used && s_ns[i].part == part && strcmp(s_ns[i].ns, ns) == 0) {
            return true;
        }
    }
    return false;
}

static void ns_add(int part, const char *ns)
{
    if (ns_exists(part, ns)) {
        return;
    }
    for (int i = 0; i < 16; i++) {
        if (!s_ns[i].used) {
            s_ns[i].used = true;
            s_ns[i].part = part;
            snprintf(s_ns[i].ns, sizeof(s_ns[i].ns), "%s", ns);
            return;
        }
    }
    abort();
}

static entry_t *ent_find(int part, const char *ns, const char *key)
{
    for (int i = 0; i < MAX_ENTRIES; i++) {
        if (s_ent[i].used && s_ent[i].part == part && strcmp(s_ent[i].ns, ns) == 0 && strcmp(s_ent[i].key, key) == 0) {
            return &s_ent[i];
        }
    }
    return NULL;
}

static entry_t *ent_new(int part, const char *ns, const char *key)
{
    entry_t *e = ent_find(part, ns, key);
    if (e) {
        return e;
    }
    for (int i = 0; i < MAX_ENTRIES; i++) {
        if (!s_ent[i].used) {
            memset(&s_ent[i], 0, sizeof(s_ent[i]));
            s_ent[i].used = true;
            s_ent[i].part = part;
            snprintf(s_ent[i].ns, sizeof(s_ent[i].ns), "%s", ns);
            snprintf(s_ent[i].key, sizeof(s_ent[i].key), "%s", key);
            ns_add(part, ns);
            return &s_ent[i];
        }
    }
    abort();
}

void fake_nvs_put_blob(const char *part, const char *ns, const char *key, const void *data, size_t len)
{
    int p = np_find(part);
    if (p < 0 || len > BLOB_CAP) {
        abort();
    }
    entry_t *e = ent_new(p, ns, key);
    e->is_u8 = false;
    memcpy(e->data, data, len);
    e->len = len;
}

void fake_nvs_put_u8(const char *part, const char *ns, const char *key, uint8_t v)
{
    int p = np_find(part);
    if (p < 0) {
        abort();
    }
    entry_t *e = ent_new(p, ns, key);
    e->is_u8 = true;
    e->data[0] = v;
    e->len = 1;
}

bool fake_nvs_get_blob(const char *part, const char *ns, const char *key, void *out, size_t *len)
{
    int p = np_find(part);
    entry_t *e = p < 0 ? NULL : ent_find(p, ns, key);
    if (!e || e->is_u8) {
        return false;
    }
    if (out) {
        memcpy(out, e->data, e->len);
    }
    if (len) {
        *len = e->len;
    }
    return true;
}

bool fake_nvs_get_u8(const char *part, const char *ns, const char *key, uint8_t *out)
{
    int p = np_find(part);
    entry_t *e = p < 0 ? NULL : ent_find(p, ns, key);
    if (!e || !e->is_u8) {
        return false;
    }
    *out = e->data[0];
    return true;
}

bool fake_nvs_has(const char *part, const char *ns, const char *key)
{
    int p = np_find(part);
    return p >= 0 && ent_find(p, ns, key) != NULL;
}

int fake_nvs_entries(const char *part)
{
    int p = np_find(part), n = 0;
    for (int i = 0; i < MAX_ENTRIES; i++) {
        n += s_ent[i].used && s_ent[i].part == p;
    }
    return n;
}

int fake_nvs_ops(fake_nvs_op_t op, const char *part)
{
    int n = 0;
    for (int i = 0; i < s_nlog; i++) {
        n += s_log[i].op == op && (part == NULL || strcmp(s_log[i].part, part) == 0);
    }
    return n;
}

int fake_nvs_ops_ns(fake_nvs_op_t op, const char *part, const char *ns)
{
    int n = 0;
    for (int i = 0; i < s_nlog; i++) {
        n += s_log[i].op == op && strcmp(s_log[i].part, part) == 0 && strcmp(s_log[i].ns, ns) == 0;
    }
    return n;
}

int fake_nvs_touches(const char *part)
{
    int n = 0;
    for (int i = 0; i < s_nlog; i++) {
        n += strcmp(s_log[i].part, part) == 0;
    }
    return n;
}

esp_err_t nvs_flash_init_partition(const char *part)
{
    logop(OP_INIT, part, NULL);
    int p = np_find(part);
    if (p < 0) {
        return ESP_ERR_NOT_FOUND;
    }
    if (s_np[p].init_err != ESP_OK) {
        return s_np[p].init_err;
    }
    s_np[p].inited = true;
    return ESP_OK;
}

esp_err_t nvs_flash_deinit_partition(const char *part)
{
    logop(OP_DEINIT, part, NULL);
    int p = np_find(part);
    if (p < 0) {
        return ESP_ERR_NOT_FOUND;
    }
    s_np[p].inited = false;
    return ESP_OK;
}

esp_err_t nvs_flash_erase_partition(const char *part)
{
    logop(OP_ERASE_PART, part, NULL);
    int p = np_find(part);
    if (p < 0) {
        return ESP_ERR_NOT_FOUND;
    }
    for (int i = 0; i < MAX_ENTRIES; i++) {
        if (s_ent[i].used && s_ent[i].part == p) {
            s_ent[i].used = false;
        }
    }
    for (int i = 0; i < 16; i++) {
        if (s_ns[i].used && s_ns[i].part == p) {
            s_ns[i].used = false;
        }
    }
    s_np[p].init_err = ESP_OK; /* a fresh partition initialises */
    s_np[p].inited = false;
    return ESP_OK;
}

esp_err_t nvs_flash_init(void)
{
    logop(OP_DEFAULT_PART, "nvs", "(default init)");
    return ESP_OK;
}

esp_err_t nvs_flash_erase(void)
{
    logop(OP_DEFAULT_PART, "nvs", "(default erase)");
    return ESP_OK;
}

esp_err_t nvs_open(const char *ns, nvs_open_mode_t mode, nvs_handle_t *h)
{
    (void)mode;
    (void)h;
    logop(OP_DEFAULT_PART, "nvs", ns);
    return ESP_OK;
}

esp_err_t nvs_open_from_partition(const char *part, const char *ns, nvs_open_mode_t mode, nvs_handle_t *h)
{
    logop(mode == NVS_READONLY ? OP_OPEN_RO : OP_OPEN_RW, part, ns);
    int p = np_find(part);
    if (p < 0) {
        return ESP_ERR_NOT_FOUND;
    }
    if (!s_np[p].inited) {
        return ESP_ERR_NVS_NOT_INITIALIZED;
    }
    if (!ns_exists(p, ns)) {
        if (mode == NVS_READONLY) {
            return ESP_ERR_NVS_NOT_FOUND;
        }
        ns_add(p, ns);
    }
    for (int i = 0; i < MAX_HANDLES; i++) {
        if (!s_h[i].used) {
            s_h[i].used = true;
            s_h[i].ro = mode == NVS_READONLY;
            s_h[i].part = p;
            snprintf(s_h[i].ns, sizeof(s_h[i].ns), "%s", ns);
            *h = (nvs_handle_t)(i + 1);
            return ESP_OK;
        }
    }
    return ESP_ERR_NO_MEM;
}

static handle_t *hnd(nvs_handle_t h)
{
    if (h < 1 || h > MAX_HANDLES || !s_h[h - 1].used) {
        abort(); /* use of a closed or bad handle is a bug in the code under test */
    }
    return &s_h[h - 1];
}

esp_err_t nvs_get_blob(nvs_handle_t h, const char *key, void *out, size_t *len)
{
    handle_t *hd = hnd(h);
    entry_t *e = ent_find(hd->part, hd->ns, key);
    if (!e || e->is_u8) {
        return ESP_ERR_NVS_NOT_FOUND;
    }
    if (out == NULL) {
        *len = e->len;
        return ESP_OK;
    }
    if (*len < e->len) {
        return ESP_ERR_NVS_INVALID_LENGTH;
    }
    memcpy(out, e->data, e->len);
    *len = e->len;
    return ESP_OK;
}

esp_err_t nvs_set_blob(nvs_handle_t h, const char *key, const void *val, size_t len)
{
    handle_t *hd = hnd(h);
    logop(OP_SET, s_np[hd->part].name, hd->ns);
    if (hd->ro) {
        return ESP_ERR_NVS_READ_ONLY;
    }
    if (len > BLOB_CAP) {
        return ESP_ERR_NVS_INVALID_LENGTH;
    }
    entry_t *e = ent_new(hd->part, hd->ns, key);
    e->is_u8 = false;
    memcpy(e->data, val, len);
    e->len = len;
    return ESP_OK;
}

esp_err_t nvs_get_u8(nvs_handle_t h, const char *key, uint8_t *out)
{
    handle_t *hd = hnd(h);
    entry_t *e = ent_find(hd->part, hd->ns, key);
    if (!e || !e->is_u8) {
        return ESP_ERR_NVS_NOT_FOUND;
    }
    *out = e->data[0];
    return ESP_OK;
}

esp_err_t nvs_set_u8(nvs_handle_t h, const char *key, uint8_t val)
{
    handle_t *hd = hnd(h);
    logop(OP_SET, s_np[hd->part].name, hd->ns);
    if (hd->ro) {
        return ESP_ERR_NVS_READ_ONLY;
    }
    entry_t *e = ent_new(hd->part, hd->ns, key);
    e->is_u8 = true;
    e->data[0] = val;
    e->len = 1;
    return ESP_OK;
}

esp_err_t nvs_erase_key(nvs_handle_t h, const char *key)
{
    handle_t *hd = hnd(h);
    logop(OP_ERASE_KEY, s_np[hd->part].name, hd->ns);
    if (hd->ro) {
        return ESP_ERR_NVS_READ_ONLY;
    }
    entry_t *e = ent_find(hd->part, hd->ns, key);
    if (!e) {
        return ESP_ERR_NVS_NOT_FOUND;
    }
    e->used = false;
    return ESP_OK;
}

esp_err_t nvs_commit(nvs_handle_t h)
{
    handle_t *hd = hnd(h);
    logop(OP_COMMIT, s_np[hd->part].name, hd->ns);
    return hd->ro ? ESP_ERR_NVS_READ_ONLY : ESP_OK;
}

void nvs_close(nvs_handle_t h)
{
    hnd(h)->used = false;
}
