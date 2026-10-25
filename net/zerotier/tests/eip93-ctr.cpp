// SPDX-License-Identifier: MPL-2.0
// Build with the patched node/AES*.cpp and Utils.cpp, -pthread, and
// -Wl,--wrap=sendmsg,--wrap=read,--wrap=socket. Run on an EIP-93 device;
// repeat with ZEROTIER_EIP93=off and on a CPU-AES host.
#include "AES.hpp"
#include <atomic>
#include <cassert>
#include <cerrno>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <dirent.h>
#include <sys/socket.h>
#include <thread>
#include <vector>

static std::atomic<unsigned> requests(0), sockets(0), completed(0), injected(0);
static thread_local int fault = 0, countdown = 0;
extern "C" ssize_t __real_sendmsg(int, const struct msghdr*, int);
extern "C" ssize_t __real_read(int, void*, size_t);
extern "C" int __real_socket(int, int, int);
extern "C" int __wrap_socket(int domain, int type, int protocol)
{
 if (domain == AF_ALG) {
  ++sockets;
  if (fault == 3) { fault = 0; ++injected; errno = EMFILE; return -1; }
 }
 return __real_socket(domain, type, protocol);
}
extern "C" ssize_t __wrap_sendmsg(int fd, const struct msghdr* msg, int flags)
{
 ++requests;
 if (fault == 1 && countdown-- == 0) { fault = 0; ++injected; errno = EIO; return -1; }
 ssize_t ret=__real_sendmsg(fd, msg, flags);
 if (fault == 4 && countdown-- == 0 && ret > 0) { fault=0; ++injected; return ret/2; }
 return ret;
}
extern "C" ssize_t __wrap_read(int fd, void* out, size_t len)
{
 ssize_t ret = __real_read(fd, out, len);
 if (fault == 2 && countdown-- == 0 && ret > 0) { fault = 0; ++injected; return ret / 2; }
 if (ret == static_cast<ssize_t>(len)) ++completed;
 return ret;
}
static unsigned fdCount()
{
 DIR* dir = opendir("/proc/self/fd");
 assert(dir);
 unsigned n = 0;
 while (readdir(dir)) ++n;
 closedir(dir);
 return n;
}
static std::vector<uint8_t> reference(const ZeroTier::AES& aes, const uint8_t* iv,
 const std::vector<uint8_t>& input)
{
 uint8_t ctr[16], stream[16];
 memcpy(ctr, iv, 16);
 auto output = input;
 for (size_t i=0; i<input.size(); i+=16) {
  aes.encrypt(ctr, stream);
  for (size_t j=0; j<16 && i+j<input.size(); ++j) output[i+j] ^= stream[j];
  for (int j=15; j>=12 && ++ctr[j]==0; --j) {}
 }
 return output;
}
static void runCase(size_t len, size_t split, bool inplace, int fail=0, int delay=0)
{
 uint8_t key[32], iv[16];
 for (unsigned i=0; i<32; ++i) key[i]=i*7+1;
 for (unsigned i=0; i<16; ++i) iv[i]=i*3;
 ZeroTier::AES aes(key);
 std::vector<uint8_t> input(len), output(len+2, 0xa5);
 for (size_t i=0; i<len; ++i) input[i]=i*13;
 auto expected=reference(aes, iv, input);
 if (inplace) memcpy(output.data()+1, input.data(), len);
 {
  ZeroTier::AES::CTR ctr(aes);
  ctr.init(iv, output.data()+1);
  fault=fail; countdown=delay;
  for (size_t pos=0; pos<len;) {
   size_t n=split ? split : len;
   if (n>len-pos) n=len-pos;
   ctr.crypt(inplace ? output.data()+1+pos : input.data()+pos, n);
   pos+=n;
  }
  ctr.finish();
  fault=0;
  assert(memcmp(expected.data(), output.data()+1, len)==0);
  assert(output.front()==0xa5 && output.back()==0xa5);
  // Reinitialize an existing CTR after changing its AES key: no stale key/fd.
  key[0]^=0xff;
  aes.init(key);
  expected=reference(aes, iv, input);
  ctr.init(iv, output.data()+1);
  ctr.crypt(input.data(), len);
  ctr.finish();
  assert(memcmp(expected.data(), output.data()+1, len)==0);
 }
}
static void gmacCase()
{
 uint8_t key[32]={};
 uint64_t tag[2], tagSW[2];
 ZeroTier::AES a(key), b(key);
 std::vector<uint8_t> plain(8193, 0x42), ciphertext(8193), sw(8193), decoded(8193);
 ZeroTier::AES::GMACSIVEncryptor enc(a,b);
 enc.init(0, ciphertext.data()); enc.update1(plain.data(), plain.size()); enc.finish1();
 enc.update2(plain.data(), plain.size()); memcpy(tag, enc.finish2(), 16);
 ZeroTier::AES::GMACSIVEncryptor fallback(a,b);
 fallback.init(0, sw.data()); fallback.update1(plain.data(), plain.size()); fallback.finish1();
 fault=1; countdown=0;
 fallback.update2(plain.data(), plain.size()); memcpy(tagSW, fallback.finish2(), 16); fault=0;
 assert(ciphertext==sw && memcmp(tag,tagSW,16)==0);
 ZeroTier::AES::GMACSIVDecryptor dec(a,b);
 dec.init(tag, decoded.data()); dec.update(ciphertext.data(), ciphertext.size()); assert(dec.finish());
 assert(decoded==plain);
}
int main(int argc, char** argv)
{
 bool requireHardware=argc>1 && strcmp(argv[1], "hardware")==0;
 unsigned baseline=fdCount();
 for (size_t len : {size_t(1),size_t(15),size_t(16),size_t(255),size_t(256),size_t(257),size_t(1500),size_t(4096),size_t(4097),size_t(8193),size_t(65537)})
  for (size_t split : {size_t(0),size_t(1),size_t(15),size_t(16),size_t(255),size_t(256),size_t(257),size_t(4096)})
   for (bool inplace : {false,true}) runCase(len, split, inplace);
 for (int fail=1; fail<=4; ++fail)
  for (int delay : {0,1})
   for (bool inplace : {false,true}) runCase(8193,0,inplace,fail,delay);
 gmacCase();
 std::vector<std::thread> threads;
 for (unsigned t=0;t<4;++t) threads.emplace_back([](){ for(unsigned i=0;i<100;++i) runCase(8193,256,true); });
 for (auto& t:threads) t.join();
 assert(fdCount()==baseline);
 if (requireHardware) assert(completed>0 && injected>=16);
 else assert(requests==0);
 printf("PASS CTR streaming, in-place, rekey, fault fallback, GMAC-SIV, concurrency, fd cleanup; requests=%u completed=%u injected=%u sockets=%u\n",requests.load(),completed.load(),injected.load(),sockets.load());
}
