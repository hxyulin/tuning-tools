// Deterministic stand-in for the public SDK 1.1 C ABI. No physical USB access.
#include <cstdint>
#include <cstring>
#include <cstdio>
#include <cstdlib>
#pragma pack(push,1)
struct Frame { uint32_t id; uint64_t timestamp; uint8_t channel, flags; uint16_t reserved; uint8_t data[64]; };
struct Baud { uint8_t channel; bool fd; uint32_t arbitration, data; float asp, dsp; };
struct Details { uint8_t channel,fd,seg1,seg2,sjw,prescaler,dseg1,dseg2,dsjw,dprescaler; };
#pragma pack(pop)
static_assert(sizeof(Frame)==80,"frame ABI");static_assert(sizeof(Baud)==18,"baud ABI");static_assert(sizeof(Details)==10,"timing ABI");
using Callback=void(*)(void*,Frame*);
struct Device { Callback rx=nullptr, tx=nullptr, err=nullptr; bool open=false,enabled[4]={}; Baud baud[4]={}; };
extern "C" {
void dmcan_context_create(void** ctx) { *ctx=new Device; }
void dmcan_context_destroy(void* ctx) { delete static_cast<Device*>(ctx); }
int dmcan_find_devices(void*) { puts("vendor diagnostic on stdout"); fflush(stdout); return 1; }
bool dmcan_device_get(void* ctx,void** out,int index) { *out=ctx; return index==0; }
void dmcan_device_get_version(void*,char* out,size_t size) { if(size>0) out[0]=0; }
bool dmcan_device_open(void* p) { static_cast<Device*>(p)->open=true;return true; }
void dmcan_device_close(void* p) { static_cast<Device*>(p)->open=false; static_cast<Device*>(p)->rx=nullptr; static_cast<Device*>(p)->tx=nullptr; static_cast<Device*>(p)->err=nullptr; }
bool dmcan_device_set_channel_baudrate(void* p,uint8_t ch,Baud b) {
    auto d=static_cast<Device*>(p);if(!d->open || ch>3 || !d->enabled[ch] || b.channel!=ch || b.arbitration==123 || b.asp!=0.75f) return false;
    d->baud[ch]=b;return true;
}
bool dmcan_device_set_channel_baudrate_details(void* p,uint8_t ch,Details d) { return static_cast<Device*>(p)->open && ch<4 && static_cast<Device*>(p)->enabled[ch] && d.channel==ch && d.seg1==13 && d.seg2==2 && d.sjw==1 && d.prescaler==4; }
bool dmcan_device_enable_channel(void* p,uint8_t ch) { if(ch>3 || static_cast<Device*>(p)->rx) return false; static_cast<Device*>(p)->enabled[ch]=true;return true; }
bool dmcan_device_disable_channel(void* p,uint8_t ch) { if(ch>3) return false; static_cast<Device*>(p)->enabled[ch]=false;return true; }
void dmcan_device_hook_recv_callback(void* p,Callback f) { static_cast<Device*>(p)->rx=f; }
void dmcan_device_hook_sent_callback(void* p,Callback f) { static_cast<Device*>(p)->tx=f; }
void dmcan_device_hook_err_callback(void* p,Callback f) { static_cast<Device*>(p)->err=f; }
bool dmcan_device_send_can(void* p,uint8_t ch,uint32_t id,bool fd,bool ext,bool rtr,bool brs,uint8_t len,uint8_t* data) {
    if(id==0x6ff) std::_Exit(7);
    auto d=static_cast<Device*>(p); if(!d->open || ch>3 || !d->enabled[ch] || len>64) return false;
    uint8_t dlc=len; if(len>8) { uint8_t lengths[]={12,16,20,24,32,48,64}; for(int i=0;i<7;i++) if(lengths[i]==len) dlc=9+i; }
    Frame f{};f.id=id|(uint32_t(ext)<<30)|(uint32_t(rtr)<<31);f.timestamp=UINT64_MAX;f.channel=ch;f.flags=(dlc<<4)|fd|(brs<<2)|2;std::memcpy(f.data,data,len);
    if(d->tx)d->tx(p,&f); f.flags&=~2; if(d->rx)d->rx(p,&f);return true;
}
}
