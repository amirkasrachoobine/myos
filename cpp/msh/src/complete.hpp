#pragma once

#include <pwd.h>

#include "lineedit.hpp"
#include "shell.hpp"

namespace msh {

Completion complete_line(Shell& sh, const std::string& line, size_t cursor);

}  // namespace msh
