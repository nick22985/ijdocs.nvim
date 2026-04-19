local M = {
	author = "nick22985",
}

local config = {
	url = "http://127.0.0.1:63343",
	server_bin = nil,
	auto_start = true,
	browser_cmd = nil,
	float = {
		border = "rounded",
		max_width = 100,
		max_height = 30,
	},
}

local server_started = false

local function get_plugin_root()
	return vim.fn.fnamemodify(debug.getinfo(1, "S").source:sub(2), ":h:h:h")
end

local function find_server_bin()
	if config.server_bin then
		return config.server_bin
	end
	local plugin_root = get_plugin_root()
	local release = plugin_root .. "/target/release/ijdocs-server"
	if vim.fn.executable(release) == 1 then
		return release
	end
	local debug_bin = plugin_root .. "/target/debug/ijdocs-server"
	if vim.fn.executable(debug_bin) == 1 then
		return debug_bin
	end
	return nil
end

local function ensure_built(callback)
	if find_server_bin() then
		return callback()
	end
	if vim.fn.executable("cargo") ~= 1 then
		vim.notify("ijdocs: cargo not found, cannot build server binary", vim.log.levels.ERROR)
		return
	end
	local plugin_root = get_plugin_root()
	vim.notify("ijdocs: building server binary (cargo build --release)...", vim.log.levels.INFO)
	vim.system({ "cargo", "build", "--release" }, { cwd = plugin_root, text = true }, function(obj)
		vim.schedule(function()
			if obj.code ~= 0 then
				vim.notify("ijdocs: build failed:\n" .. (obj.stderr or ""), vim.log.levels.ERROR)
				return
			end
			vim.notify("ijdocs: build complete", vim.log.levels.INFO)
			callback()
		end)
	end)
end

local function ensure_server()
	if server_started or not config.auto_start then
		return
	end
	server_started = true
	vim.system({ "curl", "-fsSL", "--connect-timeout", "1", config.url .. "/health" }, { text = true }, function(obj)
		vim.schedule(function()
			if obj.code == 0 then
				return
			end
			local bin = find_server_bin()
			if not bin then
				vim.notify("ijdocs: server binary not found, run `cargo build`", vim.log.levels.WARN)
				server_started = false
				return
			end
			vim.system({ bin }, { detach = true })
		end)
	end)
end

function M.setup(opts)
	config = vim.tbl_deep_extend("force", config, opts or {})
	ensure_built(ensure_server)
end

function M.config()
	return config
end

local function get_jdtls_client(bufnr)
	local clients = vim.lsp.get_clients({ bufnr = bufnr, name = "jdtls" })
	if #clients == 0 then
		clients = vim.lsp.get_clients({ bufnr = bufnr, name = "jdt.ls" })
	end
	return clients[1]
end

local function jdt_uri_to_fqn(uri)
	if not uri or not uri:match("^jdt://") then
		return nil, nil
	end
	local jar, pkg, class = uri:match("jdt://contents/([^/]+)/([^/]+)/([^/?]+)%.class")
	if not pkg or not class then
		return nil, nil
	end
	class = class:gsub("%$", ".")
	return pkg .. "." .. class, jar
end

local function file_uri_to_fqn(uri, root_markers)
	local p = uri:gsub("^file://", "")
	local m = p:match(".*/src/[^/]+/java/(.*)%.java$") or p:match(".*/src/(.*)%.java$")
	if not m then
		return nil
	end
	return (m:gsub("/", "."))
end

local function resolve_symbol(bufnr, callback)
	local client = get_jdtls_client(bufnr)
	if not client then
		return callback(nil, "no jdtls client attached")
	end
	local params = vim.lsp.util.make_position_params(0, client.offset_encoding or "utf-16")
	client:request("textDocument/definition", params, function(err, result)
		if err then
			return callback(nil, tostring(err))
		end
		if not result or vim.tbl_isempty(result) then
			return callback(nil, "no definition found")
		end
		local first = vim.islist(result) and result[1] or result
		local uri = first.uri or first.targetUri
		if not uri then
			return callback(nil, "definition has no uri")
		end
		local jdt_fqn, jar_hint = jdt_uri_to_fqn(uri)
		if jdt_fqn then
			return callback({ fqn = jdt_fqn, source = "jar", jar_hint = jar_hint }, nil)
		end
		local file_fqn = file_uri_to_fqn(uri)
		if file_fqn then
			local file_path = uri:gsub("^file://", "")
			return callback({ fqn = file_fqn, source = "file", file_path = file_path }, nil)
		end
		callback(nil, "could not parse fqn from " .. uri)
	end, bufnr)
end

local function http_get(url, callback)
	vim.system({ "curl", "-fsSL", url }, { text = true }, function(obj)
		vim.schedule(function()
			if obj.code ~= 0 then
				return callback(nil, obj.stderr ~= "" and obj.stderr or ("curl exit " .. obj.code))
			end
			callback(obj.stdout, nil)
		end)
	end)
end

local nav_history = {}

local function fetch_fqn_docs(fqn, member, jar_hint, callback)
	local url = config.url .. "/md/" .. fqn
	local sep = "?"
	if member and member ~= "" then
		url = url .. sep .. "member=" .. member
		sep = "&"
	end
	if jar_hint and jar_hint ~= "" then
		url = url .. sep .. "jar=" .. vim.uri_encode(jar_hint, "rfc2396")
	end
	http_get(url, function(body, herr)
		if not body then
			return callback(nil, herr)
		end
		local ok_json, data = pcall(vim.json.decode, body)
		if not ok_json then
			return callback(nil, "invalid response")
		end
		callback(data, nil)
	end)
end

local ijdocs_float_win = nil
local ijdocs_current_refs = {}
local ijdocs_current_data = nil

local function open_float(data)
	local lines = {}

	table.insert(lines, data.fqn)
	table.insert(lines, "")

	local in_code = false
	local has_sig = false
	for line in data.markdown:gmatch("[^\n]+") do
		if line:match("^###+ .+ ###+ *$") then
			goto continue
		end
		if line:match("^```") then
			if not in_code then
				table.insert(lines, "```java")
				in_code = true
				has_sig = true
			else
				table.insert(lines, "```")
				in_code = false
			end
			goto continue
		end
		if not in_code then
			line = line:gsub("%*%*(.-)%*%*", "%1")
			line = line:gsub("%*(.-)%*", "%1")
		end
		table.insert(lines, line)
		::continue::
	end
	if in_code then
		table.insert(lines, "```")
	end

	if not has_sig then
		local new_lines = {}
		local in_sig = false
		for _, line in ipairs(lines) do
			local is_sig = line:match("^%s*public%s")
				or line:match("^%s*private%s")
				or line:match("^%s*protected%s")
				or line:match("^%s*void%s")
				or line:match("^%s*static%s")
				or line:match("^%s*abstract%s")
				or line:match("^%s*class%s")
				or line:match("^%s*interface%s")
				or line:match("^@%w+$")
				or line:match("^@%w+ ")
				or (line:match("^%u[%w_<>%[%]%.]+%s+%w+%(") and not line:match("^%-"))
			if is_sig and not in_sig then
				table.insert(new_lines, "```java")
				in_sig = true
			elseif not is_sig and in_sig then
				table.insert(new_lines, "```")
				in_sig = false
			end
			table.insert(new_lines, line)
		end
		if in_sig then
			table.insert(new_lines, "```")
		end
		lines = new_lines
	end

	while #lines > 2 and lines[3] == "" do
		table.remove(lines, 3)
	end
	local sig_label = nil
	for _, l in ipairs(lines) do
		local method_call = l:match("(%a[%w_]*%b())")
		if method_call and not l:match("^%-") and not l:match("^_") then
			local name, params = method_call:match("^(%a[%w_]*)%((.*)%)$")
			if name and name ~= "no" and name ~= "class" and name ~= "interface" and name ~= "enum" then
				local simple_params = {}
				for p in params:gmatch("[^,]+") do
					p = p:gsub("@%w+%s*", "")
					local parts = {}
					for w in p:gmatch("%S+") do
						table.insert(parts, w)
					end
					if #parts >= 2 then
						table.insert(simple_params, parts[#parts - 1])
					elseif #parts == 1 then
						table.insert(simple_params, parts[1])
					end
				end
				sig_label = name .. "(" .. table.concat(simple_params, ", ") .. ")"
				break
			end
		end
	end

	table.insert(lines, "")
	local is_source = data.file_path ~= nil
	local action = is_source and "Enter to open file" or "Enter to open in browser"
	local link_text = (sig_label or data.fqn) .. " — " .. action
	table.insert(lines, link_text)
	local link_lnum = #lines
	local footer_highlights = { { link_lnum, 0, #link_text, "Underlined" } }
	if data.maven_coord then
		local maven_line = "Maven: " .. data.maven_coord
		table.insert(lines, maven_line)
		table.insert(footer_highlights, { #lines, 0, #maven_line, "Comment" })
	end

	if ijdocs_float_win and vim.api.nvim_win_is_valid(ijdocs_float_win) then
		vim.api.nvim_win_close(ijdocs_float_win, true)
	end

	local buf = vim.api.nvim_create_buf(false, true)
	vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
	vim.bo[buf].modifiable = false
	vim.bo[buf].filetype = "markdown"
	pcall(vim.treesitter.start, buf, "markdown")

	local ns = vim.api.nvim_create_namespace("ijdocs")
	vim.api.nvim_buf_add_highlight(buf, ns, "Title", 0, 0, -1)
	for _, r in ipairs(footer_highlights) do
		vim.api.nvim_buf_add_highlight(buf, ns, r[4], r[1] - 1, r[2], r[3])
	end

	local type_refs = data.type_refs or {}
	for lnum, line in ipairs(lines) do
		for word, _ in pairs(type_refs) do
			if not word:match("^@") then
				local start = 0
				while true do
					local s, e = line:find(word, start + 1, true)
					if not s then
						break
					end
					local before = s > 1 and line:sub(s - 1, s - 1) or " "
					local after = e < #line and line:sub(e + 1, e + 1) or " "
					if not before:match("[%w_]") and not after:match("[%w_]") then
						vim.api.nvim_buf_add_highlight(buf, ns, "Function", lnum - 1, s - 1, e)
					end
					start = e
				end
			end
		end
	end

	local max_w = config.float.max_width
	local max_h = config.float.max_height
	local width = 0
	for _, l in ipairs(lines) do
		width = math.max(width, #l)
	end
	width = math.min(width + 2, max_w)
	local height = math.min(#lines, max_h)

	local win = vim.api.nvim_open_win(buf, true, {
		relative = "cursor",
		row = 1,
		col = 0,
		width = width,
		height = height,
		style = "minimal",
		border = config.float.border,
	})

	vim.wo[win].wrap = true
	vim.wo[win].conceallevel = 3
	vim.wo[win].concealcursor = "niv"

	ijdocs_float_win = win
	ijdocs_float_buf = buf
	ijdocs_current_refs = data.type_refs or {}
	ijdocs_current_data = data

	local function navigate_type()
		local line = vim.api.nvim_get_current_line()
		local col = vim.api.nvim_win_get_cursor(0)[2] + 1
		local s, e = col, col
		while s > 1 and line:sub(s - 1, s - 1):match("[%w_%.]") do
			s = s - 1
		end
		while e < #line and line:sub(e + 1, e + 1):match("[%w_%.]") do
			e = e + 1
		end
		local compound = line:sub(s, e):gsub("^%.+", ""):gsub("%.+$", "")
		local word = vim.fn.expand("<cword>")
		local refs = ijdocs_current_refs
		local cur_line = vim.api.nvim_win_get_cursor(0)[1]
		local target_fqn = refs[compound] or refs[word] or refs["@" .. word]
		if not target_fqn then
			local suffix = "%." .. word:gsub("([%(%)%.%+%-%*%?%[%]%^%$%%])", "%%%1") .. "$"
			for k, v in pairs(refs) do
				if k:match(suffix) then
					target_fqn = v
					break
				end
			end
		end
		if not target_fqn and cur_line == 1 and ijdocs_current_data then
			target_fqn = ijdocs_current_data.fqn
		end
		if not target_fqn and ijdocs_current_data and word:match("^%l") then
			local current_fqn = ijdocs_current_data.fqn
			local cursor_pos = vim.api.nvim_win_get_cursor(0)
			table.insert(nav_history, { data = ijdocs_current_data, cursor = cursor_pos })
			fetch_fqn_docs(current_fqn, word, nil, function(new_data, err)
				if not new_data then
					table.remove(nav_history)
					vim.notify("ijdocs: " .. err, vim.log.levels.WARN)
					return
				end
				open_float(new_data)
			end)
			return
		end
		if not target_fqn then
			local navigable = {}
			for k, _ in pairs(refs) do
				if not k:match("^@") then
					table.insert(navigable, k)
				end
			end
			if #navigable > 0 then
				table.sort(navigable)
				vim.notify(
					"ijdocs: '" .. word .. "' not navigable. Try: " .. table.concat(navigable, ", "),
					vim.log.levels.INFO
				)
			else
				vim.notify("ijdocs: no navigable types in this doc", vim.log.levels.INFO)
			end
			return
		end
		local cursor_pos = vim.api.nvim_win_get_cursor(0)
		table.insert(nav_history, { data = ijdocs_current_data, cursor = cursor_pos })
		fetch_fqn_docs(target_fqn, nil, nil, function(new_data, err)
			if not new_data then
				table.remove(nav_history)
				vim.notify("ijdocs: " .. err, vim.log.levels.WARN)
				return
			end
			open_float(new_data)
		end)
	end

	local function close_float()
		nav_history = {}
		ijdocs_current_refs = {}
		ijdocs_current_data = nil
		if ijdocs_float_win and vim.api.nvim_win_is_valid(ijdocs_float_win) then
			vim.api.nvim_win_close(ijdocs_float_win, true)
		end
		ijdocs_float_win = nil
		ijdocs_float_buf = nil
	end

	local function go_back()
		if #nav_history == 0 then
			vim.notify("ijdocs: no history", vim.log.levels.INFO)
			return
		end
		local entry = table.remove(nav_history)
		open_float(entry.data)
		if entry.cursor and ijdocs_float_win and vim.api.nvim_win_is_valid(ijdocs_float_win) then
			local line_count = vim.api.nvim_buf_line_count(vim.api.nvim_win_get_buf(ijdocs_float_win))
			local row = math.min(entry.cursor[1], line_count)
			pcall(vim.api.nvim_win_set_cursor, ijdocs_float_win, { row, entry.cursor[2] })
		end
	end

	local function browse_type()
		local d = ijdocs_current_data
		if not d then
			return
		end
		if d.file_path then
			close_float()
			vim.cmd("edit " .. vim.fn.fnameescape(d.file_path))
			local class_name = d.fqn:match("%.([^%.]+)$") or d.fqn
			local patterns = {
				"class%s+" .. vim.pesc(class_name),
				"interface%s+" .. vim.pesc(class_name),
				"enum%s+" .. vim.pesc(class_name),
				"record%s+" .. vim.pesc(class_name),
				"@interface%s+" .. vim.pesc(class_name),
			}
			local lines = vim.api.nvim_buf_get_lines(0, 0, -1, false)
			for i, line in ipairs(lines) do
				for _, pat in ipairs(patterns) do
					if line:match(pat) then
						vim.api.nvim_win_set_cursor(0, { i, 0 })
						vim.cmd("normal! zz")
						return
					end
				end
			end
			return
		end
		local word = vim.fn.expand("<cword>")
		local refs = ijdocs_current_refs
		local target_fqn = refs[word] or refs["@" .. word] or d.fqn
		local url = config.url .. "/html/" .. target_fqn
		local cmd = config.browser_cmd or (vim.fn.has("mac") == 1 and "open" or "xdg-open")
		vim.system({ cmd, url }, { detach = true })
	end

	local bopts = { buffer = buf, silent = true }
	vim.keymap.set(
		"n",
		"K",
		navigate_type,
		vim.tbl_extend("force", bopts, {
			desc = "ijdocs: navigate to type docs",
		})
	)
	vim.keymap.set(
		"n",
		"gK",
		browse_type,
		vim.tbl_extend("force", bopts, {
			desc = "ijdocs: open type in browser",
		})
	)
	vim.keymap.set(
		"n",
		"<BS>",
		go_back,
		vim.tbl_extend("force", bopts, {
			desc = "ijdocs: go back",
		})
	)
	vim.keymap.set(
		"n",
		"q",
		close_float,
		vim.tbl_extend("force", bopts, {
			desc = "ijdocs: close float",
		})
	)
	vim.keymap.set(
		"n",
		"<CR>",
		browse_type,
		vim.tbl_extend("force", bopts, {
			desc = "ijdocs: open in browser",
		})
	)
	vim.keymap.set(
		"n",
		"<Esc>",
		close_float,
		vim.tbl_extend("force", bopts, {
			desc = "ijdocs: close float",
		})
	)
end

local function fetch_source_docs(file_path, member, fqn, callback)
	local url = config.url .. "/source?path=" .. vim.uri_encode(file_path, "rfc2396")
	if member and member ~= "" then
		url = url .. "&member=" .. vim.uri_encode(member, "rfc2396")
	end
	http_get(url, function(body, herr)
		if not body then
			return callback(nil, herr)
		end
		local ok_json, data = pcall(vim.json.decode, body)
		if not ok_json then
			return callback(nil, "invalid response")
		end
		data.fqn = fqn
		data.file_path = file_path
		callback(data, nil)
	end)
end

function M.hover()
	local bufnr = vim.api.nvim_get_current_buf()
	local word = vim.fn.expand("<cword>")
	resolve_symbol(bufnr, function(sym, err)
		if not sym then
			vim.notify("ijdocs: " .. err, vim.log.levels.WARN)
			return
		end
		local function show(data, herr)
			if not data then
				vim.lsp.buf.hover()
				return
			end
			open_float(data)
		end
		local member = word
		local class_name = sym.fqn:match("%.([^%.]+)$") or sym.fqn
		if member == class_name then
			member = nil
		end
		if sym.source == "jar" then
			fetch_fqn_docs(sym.fqn, member, sym.jar_hint, show)
		else
			fetch_source_docs(sym.file_path, member, sym.fqn, show)
		end
	end)
end

function M.show_float(data)
	open_float(data)
end

function M.tree()
	require("ijdocs.tree").toggle()
end

function M.tree_browser()
	local url = config.url .. "/tree/html"
	local cmd = config.browser_cmd or (vim.fn.has("mac") == 1 and "open" or "xdg-open")
	vim.system({ cmd, url }, { detach = true })
end

function M.open_browser()
	local bufnr = vim.api.nvim_get_current_buf()
	resolve_symbol(bufnr, function(sym, err)
		if not sym then
			vim.notify("ijdocs: " .. err, vim.log.levels.WARN)
			return
		end
		if sym.source == "file" then
			vim.cmd("edit " .. vim.fn.fnameescape(sym.file_path))
			return
		end
		local url = config.url .. "/html/" .. sym.fqn
		local cmd = config.browser_cmd or (vim.fn.has("mac") == 1 and "open" or "xdg-open")
		vim.system({ cmd, url }, { detach = true })
	end)
end

return M
