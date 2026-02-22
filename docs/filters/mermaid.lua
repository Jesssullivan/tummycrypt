-- mermaid.lua — Pandoc Lua filter to render Mermaid diagrams as SVG
-- Requires mmdc (mermaid-cli) on PATH. Falls back to code block if unavailable.

local system = require("pandoc.system")

local function file_exists(name)
  local f = io.open(name, "r")
  if f then
    f:close()
    return true
  end
  return false
end

local mmdc_available = nil

local function check_mmdc()
  if mmdc_available == nil then
    local ok = os.execute("mmdc --version >/dev/null 2>&1")
    mmdc_available = (ok == true or ok == 0)
    if not mmdc_available then
      io.stderr:write("mermaid.lua: mmdc not found, rendering diagrams as code blocks\n")
    end
  end
  return mmdc_available
end

local img_counter = 0

function CodeBlock(block)
  if block.classes[1] ~= "mermaid" then
    return nil
  end

  if not check_mmdc() then
    return nil
  end

  img_counter = img_counter + 1

  return system.with_temporary_directory("mermaid", function(tmpdir)
    local input_file = tmpdir .. "/input.mmd"
    local output_file = tmpdir .. "/output.svg"

    local f = io.open(input_file, "w")
    f:write(block.text)
    f:close()

    local cmd = string.format(
      "mmdc -i %s -o %s -b transparent --quiet 2>/dev/null",
      input_file, output_file
    )
    os.execute(cmd)

    if file_exists(output_file) then
      local img_f = io.open(output_file, "r")
      local svg_data = img_f:read("*a")
      img_f:close()
      return pandoc.RawBlock("latex",
        "\\begin{center}\n" ..
        "\\includegraphics[width=\\textwidth]{" .. output_file .. "}\n" ..
        "\\end{center}"
      )
    else
      return nil
    end
  end)
end
