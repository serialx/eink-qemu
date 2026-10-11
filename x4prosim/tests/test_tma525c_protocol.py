"""Compile and exercise the production TMA525C I2C protocol without QEMU."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
PRELUDE = r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
typedef struct {} I2CSlave;
typedef int QemuInputHandlerState;
typedef struct { bool pending; int64_t deadline; void (*cb)(void *); void *opaque; } QEMUTimer;
typedef int *qemu_irq;
typedef void DeviceState;
typedef void QemuConsole;
enum { INPUT_EVENT_KIND_ABS, INPUT_EVENT_KIND_BTN, INPUT_AXIS_X, INPUT_AXIS_Y,
       INPUT_BUTTON_LEFT, INPUT_EVENT_ABS_MIN=0, INPUT_EVENT_ABS_MAX=32767 };
typedef struct { int axis, value; } InputMoveEvent;
typedef struct { int type; InputMoveEvent abs; struct { int button; bool down; } btn; } QemuInputEvent;
static int qemu_input_scale_axis(int v,int a,int b,int c,int d) { return c+(int64_t)(v-a)*(d-c)/(b-a); }
typedef struct TMA525CState TMA525CState;
#define TMA525C(p) ((TMA525CState *)(p))
#define ARRAY_SIZE(a) (sizeof(a) / sizeof((a)[0]))
#define QEMU_CLOCK_VIRTUAL 0
enum i2c_event { I2C_START_SEND, I2C_START_RECV, I2C_FINISH, I2C_NACK };
static void qemu_set_irq(qemu_irq irq, int level) { if (irq) *irq = level; }
static void stw_le_p(void *p, uint16_t v) { uint8_t *b=p; b[0]=v; b[1]=v>>8; }
static uint16_t lduw_le_p(const void *p) { const uint8_t *b=p; return b[0] | b[1]<<8; }
static int64_t virtual_ms;
static int64_t qemu_clock_get_ms(int clock) { return virtual_ms; }
static void timer_mod(QEMUTimer *timer, int64_t deadline) {
    assert(timer); timer->pending=true; timer->deadline=deadline;
}
static void timer_del(QEMUTimer *timer) { if(timer) timer->pending=false; }
'''
CASES = r'''
static void write_packet(TMA525CState *s, const uint8_t *bytes, int n)
{
    assert(tma525c_event(&s->parent_obj, I2C_START_SEND)==0);
    for(int i=0;i<n;i++) assert(tma525c_send(&s->parent_obj, bytes[i])==0);
    tma525c_event(&s->parent_obj, I2C_FINISH);
}
static void read_packet(TMA525CState *s, uint8_t *bytes, int n)
{
    assert(tma525c_event(&s->parent_obj, I2C_START_RECV)==0);
    for(int i=0;i<n;i++) bytes[i]=tma525c_recv(&s->parent_obj);
    tma525c_event(&s->parent_obj, I2C_NACK);
    tma525c_event(&s->parent_obj, I2C_FINISH);
}
static void get_packet(TMA525CState *s, uint8_t *bytes, int expected)
{
    read_packet(s,bytes,2);
    assert(lduw_le_p(bytes)==expected);
    if(expected) read_packet(s,bytes,expected);
}
static TMA525CState *device(int *irq)
{
    TMA525CState *s=calloc(1,sizeof(*s));
    s->power=s->rst=true; s->irq=irq; s->fw_major=1;s->fw_minor=10;
    s->host_touch_ms=100;
    s->tap_timer=calloc(1,sizeof(QEMUTimer));
    s->host_timer=calloc(1,sizeof(QEMUTimer));
    s->host_timer->cb=tma525c_host_release; s->host_timer->opaque=s;
    tma525c_reset_protocol(s);
    return s;
}
static void boot(TMA525CState *s)
{
    uint8_t reply[64], launch[]={4,0,11,0,0x40,0,1,0x3b,0,0,0,0,0x17};
    get_packet(s,reply,0);
    write_packet(s,(uint8_t[]){1,0},2);
    get_packet(s,reply,32);
    assert(reply[2]==0xff);
    stw_le_p(launch+10,tma525c_crc16(launch+6,4));
    write_packet(s,launch,sizeof(launch));
    get_packet(s,reply,0);
    write_packet(s,(uint8_t[]){1,0},2);
    get_packet(s,reply,32);
    assert(reply[2]==0xf7 && lduw_le_p(reply+4)==0x100);
    write_packet(s,(uint8_t[]){4,0,5,0,0x2f,0,2},7);
    get_packet(s,reply,51);
    assert(reply[2]==0x1f && reply[4]==2 && reply[9]==1 && reply[10]==10);
    assert(lduw_le_p(reply+39)==480 && lduw_le_p(reply+41)==800);
}
static void host_button(TMA525CState *s, bool down)
{
    QemuInputEvent event={.type=INPUT_EVENT_KIND_BTN,
        .btn={.button=INPUT_BUTTON_LEFT,.down=down}};
    tma525c_input_event(s,NULL,&event);
    tma525c_input_sync(s);
}
static void host_move(TMA525CState *s, int x, int y)
{
    QemuInputEvent event={.type=INPUT_EVENT_KIND_ABS};
    event.abs=(InputMoveEvent){INPUT_AXIS_X,x}; tma525c_input_event(s,NULL,&event);
    event.abs=(InputMoveEvent){INPUT_AXIS_Y,y}; tma525c_input_event(s,NULL,&event);
    tma525c_input_sync(s);
}
static void advance(TMA525CState *s, int64_t now)
{
    assert(now>=virtual_ms); virtual_ms=now;
    QEMUTimer *timer=s->host_timer;
    if(timer->pending && timer->deadline<=now) {
        timer->pending=false; timer->cb(timer->opaque);
    }
}
int main(int argc,char **argv)
{
    int irq=1; uint8_t bytes[64]; TMA525CState *s=device(&irq);
    assert(irq==0); boot(s); assert(irq==1);
    if(!strcmp(argv[1],"input")) {
        tma525c_set_contact(s,true,123,456);
        assert(irq==0);
        read_packet(s,bytes,2);
        assert(lduw_le_p(bytes)==17 && irq==0); /* size peek must retain report */
        read_packet(s,bytes,17);
        assert(bytes[2]==1 && bytes[5]==1 && lduw_le_p(bytes+9)==123 && lduw_le_p(bytes+11)==456);
        assert(irq==1);
        tma525c_set_contact(s,true,130,460);
        tma525c_set_contact(s,false,130,460);
        get_packet(s,bytes,17); assert(lduw_le_p(bytes+9)==130);
        assert(irq==0); get_packet(s,bytes,7); assert(bytes[5]==0 && irq==1);
    } else if(!strcmp(argv[1],"burst")) {
        /* Both host events may arrive before the guest services INT. The
         * stock driver drains reports while INT remains low, so retain the
         * press until its complete read, then expose the queued release. */
        tma525c_set_contact(s,true,240,56);
        tma525c_set_contact(s,false,240,56);
        assert(irq==0 && s->count==2);
        get_packet(s,bytes,17);
        assert(bytes[5]==1 && lduw_le_p(bytes+9)==240 && lduw_le_p(bytes+11)==56);
        assert(irq==0 && s->count==1);
        get_packet(s,bytes,7);
        assert(bytes[5]==0 && irq==1 && s->count==0);
        read_packet(s,bytes,2);
        assert(lduw_le_p(bytes)==2 && irq==1);
    } else if(!strcmp(argv[1],"shortclick")) {
        host_button(s,true);
        get_packet(s,bytes,17);
        virtual_ms=20;
        host_button(s,false);
        read_packet(s,bytes,2);
        assert(lduw_le_p(bytes)==2 && s->touch && irq==1);
        advance(s,99);
        assert(s->touch && irq==1);
        advance(s,100);
        get_packet(s,bytes,7);
        assert(!s->touch && !s->host_timer->pending && irq==1);
    } else if(!strcmp(argv[1],"longclick")) {
        host_button(s,true); get_packet(s,bytes,17);
        advance(s,150); host_button(s,false);
        get_packet(s,bytes,7);
        assert(!s->touch && !s->host_timer->pending);
    } else if(!strcmp(argv[1],"nohold")) {
        s->host_touch_ms=0;
        host_button(s,true); host_button(s,false);
        get_packet(s,bytes,17); get_packet(s,bytes,7);
        assert(!s->touch && !s->host_timer->pending);
    } else if(!strcmp(argv[1],"holdmove")) {
        host_move(s,0,0); host_button(s,true); get_packet(s,bytes,17);
        advance(s,10); host_button(s,false);
        host_move(s,32767,32767);
        read_packet(s,bytes,2); assert(lduw_le_p(bytes)==2);
        assert(s->x==0 && s->y==0); /* viewport/hover cannot drag retained press */
        advance(s,30); host_button(s,true);
        get_packet(s,bytes,7); get_packet(s,bytes,17);
        assert(lduw_le_p(bytes+9)==479 && lduw_le_p(bytes+11)==799);
        assert(!s->host_timer->pending && s->host_deadline==130);
        host_button(s,false); advance(s,100);
        assert(s->touch && irq==1);
        advance(s,130); get_packet(s,bytes,7); assert(!s->touch);
    } else if(!strcmp(argv[1],"idleqom")) {
        /* Raw QOM contacts are independent of idle host hover/viewport ABS
         * events. Coordinates still stage for the next host button press. */
        tma525c_set_contact(s,true,123,456); get_packet(s,bytes,17);
        host_move(s,32767,32767);
        assert(s->touch && s->x==123 && s->y==456);
        assert(s->host_x==479 && s->host_y==799);
        read_packet(s,bytes,2); assert(lduw_le_p(bytes)==2 && irq==1);
        tma525c_set_contact(s,false,123,456); get_packet(s,bytes,7);
        host_button(s,true); get_packet(s,bytes,17);
        assert(lduw_le_p(bytes+9)==479 && lduw_le_p(bytes+11)==799);
    } else if(!strcmp(argv[1],"holdreset")) {
        host_button(s,true); get_packet(s,bytes,17);
        host_button(s,false); assert(s->host_timer->pending);
        tma525c_reset_protocol(s);
        assert(!s->host_timer->pending && !s->host_release_pending && !s->host_down);
        boot(s); advance(s,200);
        assert(!s->touch && irq==1);
    } else if(!strcmp(argv[1],"power")) {
        write_packet(s,(uint8_t[]){5,0,1,8},4);
        get_packet(s,bytes,5); assert(bytes[2]==0xf0 && bytes[3]==1 && bytes[4]==8);
        tma525c_set_contact(s,true,10,20); assert(irq==1);
        write_packet(s,(uint8_t[]){5,0,0,8},4);
        get_packet(s,bytes,5); get_packet(s,bytes,17); assert(bytes[5]==1);
        tma525c_rst_in(s,0,0); assert(irq==1);
        assert(tma525c_event(&s->parent_obj,I2C_START_RECV)==-1);
        tma525c_rst_in(s,0,1); boot(s);
        tma525c_power_in(s,0,0);
        assert(tma525c_event(&s->parent_obj,I2C_START_SEND)==-1);
    } else if(!strcmp(argv[1],"malformed")) {
        write_packet(s,(uint8_t[]){4,0,50,0,0x2f,0,2},7);
        assert(irq==1); read_packet(s,bytes,2); assert(lduw_le_p(bytes)==2);
        write_packet(s,(uint8_t[]){4,0,5,0,0x2f,0,0x7f},7);
        get_packet(s,bytes,6); assert(bytes[4]==0x7f && bytes[5]!=0);
        assert(tma525c_event(&s->parent_obj,I2C_START_SEND)==0);
        for(int i=0;i<300;i++) tma525c_send(&s->parent_obj,0);
        tma525c_event(&s->parent_obj,I2C_FINISH);
        assert(irq==1);
    } else if(!strcmp(argv[1],"host")) {
        s->invert_x=s->invert_y=true;
        QemuInputEvent event={.type=INPUT_EVENT_KIND_BTN,
            .btn={.button=INPUT_BUTTON_LEFT,.down=true}};
        tma525c_input_event(s,NULL,&event);
        event.type=INPUT_EVENT_KIND_ABS;
        event.abs=(InputMoveEvent){INPUT_AXIS_X,0}; tma525c_input_event(s,NULL,&event);
        event.abs=(InputMoveEvent){INPUT_AXIS_Y,32767}; tma525c_input_event(s,NULL,&event);
        assert(irq==1); /* input changes are batched until sync */
        tma525c_input_sync(s); get_packet(s,bytes,17);
        assert(lduw_le_p(bytes+9)==479 && lduw_le_p(bytes+11)==0);
        event.type=INPUT_EVENT_KIND_BTN; event.btn.down=false;
        tma525c_input_event(s,NULL,&event); tma525c_input_sync(s);
        advance(s,100);
        get_packet(s,bytes,7); assert(bytes[5]==0);
    } else if(!strcmp(argv[1],"restart")) {
        /* Descriptor request with repeated START instead of STOP. */
        assert(tma525c_event(&s->parent_obj,I2C_START_SEND)==0);
        tma525c_send(&s->parent_obj,1);tma525c_send(&s->parent_obj,0);
        get_packet(s,bytes,32); assert(bytes[2]==0xf7);
        /* Backpressure cannot lose the final lift. */
        for(int i=0;i<20;i++) tma525c_set_contact(s,true,i,10);
        tma525c_set_contact(s,false,19,10);
        while(s->count>1) get_packet(s,bytes,17);
        get_packet(s,bytes,7); assert(bytes[5]==0 && irq==1);
    } else if(strcmp(argv[1],"boot")) abort();
    free(s->host_timer); free(s->tap_timer); free(s); return 0;
}
'''


class TMA525CProtocol(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.addClassCleanup(cls.temp.cleanup)
        source = (ROOT / 'hw/input/tma525c.c').read_text()
        core = source[source.index('#define TMA525C_MAX_PACKET'):
                      source.index('static const QemuInputHandler tma525c_input_handler')]
        path = Path(cls.temp.name) / 'protocol.c'
        cls.binary = path.with_suffix('')
        path.write_text(PRELUDE + core + CASES)
        subprocess.run([os.environ.get('CC', 'cc'), '-std=c11', str(path),
                        '-o', str(cls.binary)], check=True, capture_output=True, text=True)

    def test_boot_handshake(self):
        subprocess.run([self.binary, 'boot'], check=True)

    def test_touch_peek_move_and_release(self):
        subprocess.run([self.binary, 'input'], check=True)

    def test_sleep_reset_and_power(self):
        subprocess.run([self.binary, 'power'], check=True)

    def test_short_host_click_holds_for_guest_poll(self):
        subprocess.run([self.binary, 'shortclick'], check=True)

    def test_long_host_press_releases_immediately(self):
        subprocess.run([self.binary, 'longclick'], check=True)

    def test_host_minimum_hold_can_be_disabled(self):
        subprocess.run([self.binary, 'nohold'], check=True)

    def test_pending_release_freezes_coordinates_and_new_press_restarts_hold(self):
        subprocess.run([self.binary, 'holdmove'], check=True)

    def test_reset_cancels_pending_host_release(self):
        subprocess.run([self.binary, 'holdreset'], check=True)

    def test_idle_host_movement_does_not_move_raw_qom_contact(self):
        subprocess.run([self.binary, 'idleqom'], check=True)

    def test_press_release_queued_before_first_interrupt_service(self):
        subprocess.run([self.binary, 'burst'], check=True)

    def test_malformed_packets(self):
        subprocess.run([self.binary, 'malformed'], check=True)

    def test_host_input_coordinates_and_release(self):
        subprocess.run([self.binary, 'host'], check=True)

    def test_repeated_start_and_input_backpressure(self):
        subprocess.run([self.binary, 'restart'], check=True)
