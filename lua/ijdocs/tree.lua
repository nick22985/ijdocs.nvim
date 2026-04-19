local M = {}

local tree_buf = nil
local tree_win = nil
local tree_data = {}
local node_map = {}
local search_mode = false
local search_query = ""
local search_results = {}
local search_pkg_state = {}

local icons = {
	artifact_open = " ",
	artifact_closed = " ",
	package_open = " ",
	package_closed = " ",
	class = " ",
}

local function get_config()
	return require("ijdocs").config()
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

local function fetch_json(path, callback)
	local cfg = get_config()
	http_get(cfg.url .. path, function(body, err)
		if not body then
			return callback(nil, err)
		end
		local ok, data = pcall(vim.json.decode, body)
		if not ok then
			return callback(nil, "invalid json")
		end
		callback(data, nil)
	end)
end

local function fetch_tree(callback)
	fetch_json("/tree", callback)
end

local function fetch_classes(jar_idx, package, callback)
	fetch_json("/tree/classes?jar_idx=" .. jar_idx .. "&package=" .. vim.uri_encode(package, "rfc2396"), callback)
end

local function fetch_search(query, callback)
	fetch_json("/search?q=" .. vim.uri_encode(query, "rfc2396") .. "&limit=80", callback)
end

local function render()
	if not tree_buf or not vim.api.nvim_buf_is_valid(tree_buf) then
		return
	end

	local lines = {}
	node_map = {}

	if search_mode and search_query ~= "" then
		render_search(lines)
	else
		render_tree_view(lines)
	end

	if #lines == 0 then
		lines = { "  No results." }
		node_map = {}
	end

	vim.bo[tree_buf].modifiable = true
	vim.api.nvim_buf_set_lines(tree_buf, 0, -1, false, lines)
	vim.bo[tree_buf].modifiable = false

	local ns = vim.api.nvim_create_namespace("ijdocs_tree")
	vim.api.nvim_buf_clear_namespace(tree_buf, ns, 0, -1)
	for i, node in ipairs(node_map) do
		if node.type == "artifact" then
			vim.api.nvim_buf_add_highlight(tree_buf, ns, "Directory", i - 1, 0, -1)
		elseif node.type == "package" or node.type == "pkg_header" then
			vim.api.nvim_buf_add_highlight(tree_buf, ns, "Type", i - 1, 0, -1)
		elseif node.type == "class" then
			vim.api.nvim_buf_add_highlight(tree_buf, ns, "Function", i - 1, 0, -1)
		elseif node.type == "search_header" then
			vim.api.nvim_buf_add_highlight(tree_buf, ns, "Comment", i - 1, 0, -1)
		end
	end
end

function render_tree_view(lines)
	for _, art in ipairs(tree_data) do
		local art_icon = art.expanded and icons.artifact_open or icons.artifact_closed
		local label = art.maven_coord or art.artifact
		table.insert(lines, art_icon .. label)
		table.insert(node_map, { type = "artifact", data = art })

		if art.expanded then
			local pkgs = art.packages or {}
			for _, pkg in ipairs(pkgs) do
				local pkg_node = art._pkg_nodes and art._pkg_nodes[pkg]
				local pkg_icon = (pkg_node and pkg_node.expanded) and icons.package_open or icons.package_closed
				table.insert(lines, "  " .. pkg_icon .. pkg)
				table.insert(node_map, { type = "package", data = pkg_node or { name = pkg }, artifact = art })

				if pkg_node and pkg_node.expanded and pkg_node.classes then
					for _, cls in ipairs(pkg_node.classes) do
						table.insert(lines, "    " .. icons.class .. cls)
						table.insert(node_map, { type = "class", class = cls, package = pkg, artifact = art })
					end
				end
			end
		end
	end
end

function render_search(lines)
	local groups = {}
	local group_order = {}
	local pkg_only_hits = {}

	for _, hit in ipairs(search_results) do
		if hit.kind == "package" then
			table.insert(pkg_only_hits, hit)
		else
			local parts = vim.split(hit.fqn, ".", { plain = true })
			local pkg = table.concat(parts, ".", 1, #parts - 1)
			if not groups[pkg] then
				groups[pkg] = { pkg = pkg, artifact = hit.artifact, jar_idx = hit.jar_idx, classes = {} }
				table.insert(group_order, pkg)
			end
			table.insert(groups[pkg].classes, hit)
		end
	end

	local grouped_pkgs = {}
	for _, pkg in ipairs(group_order) do
		grouped_pkgs[pkg] = true
	end
	for _, hit in ipairs(pkg_only_hits) do
		if not grouped_pkgs[hit.fqn] then
			local state = search_pkg_state[hit.fqn]
			local expanded = state and state.expanded
			local icon = expanded and icons.package_open or icons.package_closed
			table.insert(lines, icon .. hit.fqn .. "  " .. hit.artifact)
			table.insert(
				node_map,
				{
					type = "pkg_header",
					package = hit.fqn,
					artifact_name = hit.artifact,
					jar_idx = hit.jar_idx,
					has_classes = false,
				}
			)

			if expanded and state.classes then
				for _, cls in ipairs(state.classes) do
					table.insert(lines, "  " .. icons.class .. cls)
					table.insert(
						node_map,
						{ type = "class", class = cls, package = hit.fqn, fqn = hit.fqn .. "." .. cls }
					)
				end
			end
		end
	end

	for _, pkg in ipairs(group_order) do
		local group = groups[pkg]
		local state = search_pkg_state[pkg]
		local collapsed = state and state.collapsed
		local icon = collapsed and icons.package_closed or icons.package_open
		table.insert(lines, icon .. group.pkg .. "  " .. group.artifact)
		table.insert(
			node_map,
			{
				type = "pkg_header",
				package = group.pkg,
				artifact_name = group.artifact,
				jar_idx = group.jar_idx,
				has_classes = true,
			}
		)

		if not collapsed then
			for _, hit in ipairs(group.classes) do
				local parts = vim.split(hit.fqn, ".", { plain = true })
				local short = parts[#parts]
				table.insert(lines, "  " .. icons.class .. short)
				table.insert(node_map, { type = "class", class = short, package = group.pkg, fqn = hit.fqn })
			end
		end
	end
end

local function toggle_node()
	if not tree_win or not vim.api.nvim_win_is_valid(tree_win) then
		return
	end
	local cursor = vim.api.nvim_win_get_cursor(tree_win)
	local idx = cursor[1]
	local node = node_map[idx]
	if not node then
		return
	end

	if node.type == "artifact" then
		node.data.expanded = not node.data.expanded
		render()
	elseif node.type == "package" then
		local art = node.artifact
		art._pkg_nodes = art._pkg_nodes or {}
		local pkg_name = node.data.name
		if not art._pkg_nodes[pkg_name] then
			art._pkg_nodes[pkg_name] = { name = pkg_name, expanded = false, classes = nil }
		end
		local pkg_node = art._pkg_nodes[pkg_name]

		if pkg_node.expanded then
			pkg_node.expanded = false
			render()
		else
			if pkg_node.classes then
				pkg_node.expanded = true
				render()
			else
				fetch_classes(art.jar_idx, pkg_name, function(classes, err)
					if err then
						vim.notify("ijdocs: " .. err, vim.log.levels.WARN)
						return
					end
					pkg_node.classes = classes or {}
					pkg_node.expanded = true
					render()
				end)
			end
		end
	elseif node.type == "pkg_header" then
		if node.has_classes then
			local state = search_pkg_state[node.package] or {}
			state.collapsed = not state.collapsed
			search_pkg_state[node.package] = state
			render()
		else
			local state = search_pkg_state[node.package]
			if state and state.expanded then
				state.expanded = false
				render()
			else
				if not state then
					state = { expanded = false, classes = nil }
					search_pkg_state[node.package] = state
				end
				if state.classes then
					state.expanded = true
					render()
				else
					fetch_classes(node.jar_idx, node.package, function(classes, err)
						if err then
							vim.notify("ijdocs: " .. err, vim.log.levels.WARN)
							return
						end
						state.classes = classes or {}
						state.expanded = true
						render()
					end)
				end
			end
		end
	elseif node.type == "class" then
		open_class_docs(node)
	end
end

function open_class_docs(node)
	local fqn = node.fqn or (node.package .. "." .. node.class)
	local ijdocs = require("ijdocs")
	local cfg = get_config()

	http_get(cfg.url .. "/md/" .. fqn, function(body, err)
		if not body then
			vim.notify("ijdocs: " .. (err or "unknown error"), vim.log.levels.WARN)
			return
		end
		local ok, data = pcall(vim.json.decode, body)
		if not ok then
			vim.notify("ijdocs: invalid response", vim.log.levels.WARN)
			return
		end
		data.fqn = fqn
		local prev_win = nil
		for _, win in ipairs(vim.api.nvim_list_wins()) do
			if win ~= tree_win and vim.api.nvim_win_is_valid(win) then
				local buf = vim.api.nvim_win_get_buf(win)
				if vim.bo[buf].buftype ~= "nofile" or vim.bo[buf].filetype == "markdown" then
					prev_win = win
					break
				end
			end
		end
		if prev_win then
			vim.api.nvim_set_current_win(prev_win)
		end
		ijdocs.show_float(data)
	end)
end

function do_search(query)
	search_query = query
	search_mode = true
	search_pkg_state = {}

	vim.bo[tree_buf].modifiable = true
	vim.api.nvim_buf_set_lines(tree_buf, 0, -1, false, { "  Searching..." })
	vim.bo[tree_buf].modifiable = false

	fetch_search(query, function(hits, err)
		if err then
			vim.notify("ijdocs: " .. err, vim.log.levels.WARN)
			return
		end
		search_results = hits or {}
		render()
	end)
end

local function prompt_search()
	vim.ui.input({ prompt = "ijdocs search: ", default = search_query }, function(input)
		if not input or input == "" then
			if search_mode then
				search_mode = false
				search_query = ""
				search_results = {}
				render()
			end
			return
		end
		do_search(input)
	end)
end

local function close_tree()
	if tree_win and vim.api.nvim_win_is_valid(tree_win) then
		vim.api.nvim_win_close(tree_win, true)
	end
	tree_win = nil
	tree_buf = nil
	tree_data = {}
	node_map = {}
	search_mode = false
	search_query = ""
	search_results = {}
	search_pkg_state = {}
end

local function go_back()
	if search_mode then
		search_mode = false
		search_query = ""
		search_results = {}
		search_pkg_state = {}
		render()
	end
end

function M.toggle()
	if tree_win and vim.api.nvim_win_is_valid(tree_win) then
		close_tree()
		return
	end

	tree_buf = vim.api.nvim_create_buf(false, true)
	vim.bo[tree_buf].buftype = "nofile"
	vim.bo[tree_buf].bufhidden = "wipe"
	vim.bo[tree_buf].swapfile = false
	vim.bo[tree_buf].filetype = "ijdocs_tree"

	vim.cmd("topleft 45vsplit")
	tree_win = vim.api.nvim_get_current_win()
	vim.api.nvim_win_set_buf(tree_win, tree_buf)
	vim.wo[tree_win].number = false
	vim.wo[tree_win].relativenumber = false
	vim.wo[tree_win].signcolumn = "no"
	vim.wo[tree_win].foldcolumn = "0"
	vim.wo[tree_win].winfixwidth = true
	vim.wo[tree_win].cursorline = true

	vim.bo[tree_buf].modifiable = true
	vim.api.nvim_buf_set_lines(tree_buf, 0, -1, false, { "  Loading..." })
	vim.bo[tree_buf].modifiable = false

	local bopts = { buffer = tree_buf, silent = true, nowait = true }
	vim.keymap.set("n", "<CR>", toggle_node, vim.tbl_extend("force", bopts, { desc = "ijdocs: toggle/open" }))
	vim.keymap.set("n", "o", toggle_node, vim.tbl_extend("force", bopts, { desc = "ijdocs: toggle/open" }))
	vim.keymap.set("n", "q", close_tree, vim.tbl_extend("force", bopts, { desc = "ijdocs: close tree" }))
	vim.keymap.set("n", "<Esc>", close_tree, vim.tbl_extend("force", bopts, { desc = "ijdocs: close tree" }))
	vim.keymap.set("n", "/", prompt_search, vim.tbl_extend("force", bopts, { desc = "ijdocs: search" }))
	vim.keymap.set("n", "s", prompt_search, vim.tbl_extend("force", bopts, { desc = "ijdocs: search" }))
	vim.keymap.set("n", "<BS>", go_back, vim.tbl_extend("force", bopts, { desc = "ijdocs: back to tree" }))
	vim.keymap.set("n", "R", function()
		if search_mode then
			do_search(search_query)
		else
			M.refresh()
		end
	end, vim.tbl_extend("force", bopts, { desc = "ijdocs: refresh" }))

	M.refresh()
end

function M.refresh()
	fetch_tree(function(data, err)
		if err then
			vim.notify("ijdocs: failed to load tree: " .. err, vim.log.levels.ERROR)
			return
		end
		tree_data = data or {}
		for _, art in ipairs(tree_data) do
			art.expanded = false
			art._pkg_nodes = {}
		end
		render()
	end)
end

return M
