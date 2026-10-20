// Phase 0 smoke test: proves the C++ cross toolchain and libstdc++ work.
#include <iostream>
#include <string>
#include <sys/utsname.h>

int main() {
    utsname u{};
    uname(&u);
    std::string kernel = u.release;
    std::cout << "hello from C++" << __cplusplus / 100 % 100
              << " on " << u.machine << " (kernel " << kernel << ")\n";
    return 0;
}
